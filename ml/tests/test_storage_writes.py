# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio,json,uuid
import pytest
from app import storage_writes,rag_ctx
from app.config import settings
from test_inference_target import clean_context,anyio_backend


@pytest.mark.anyio
async def test_write_failure_restores_durable_snapshot_before_lock_release(monkeypatch,tmp_path):
    monkeypatch.setattr(settings,'storage_root',str(tmp_path));kb,doc=str(uuid.uuid4()),str(uuid.uuid4());seen=[]
    async def snapshot(name,*args,**kwargs):return []
    async def restore(data):
        assert list((tmp_path/'.index-writes').glob('*.json'))
        seen.append(data)
    monkeypatch.setattr(storage_writes,'_snapshot',snapshot);monkeypatch.setattr(storage_writes,'_restore',restore)
    with pytest.raises(ValueError):
        async with storage_writes.replacement(kb,doc,['active','desired']):raise ValueError('write failed')
    assert len(seen)==1 and set(seen[0]['collections'])=={'active','desired'}
    assert not list((tmp_path/'.index-writes').glob('*.json')) and not storage_writes._owner.get()


@pytest.mark.anyio
async def test_unavailable_restore_blocks_reads_and_preserves_journal_until_recovery(monkeypatch,tmp_path):
    monkeypatch.setattr(settings,'storage_root',str(tmp_path));kb,doc=str(uuid.uuid4()),str(uuid.uuid4())
    async def snapshot(*args,**kwargs):return []
    async def unavailable(data):raise RuntimeError('storage down')
    monkeypatch.setattr(storage_writes,'_snapshot',snapshot);monkeypatch.setattr(storage_writes,'_restore',unavailable)
    with pytest.raises(storage_writes.StorageRecoveryRequired):
        async with storage_writes.replacement(kb,doc,['active']):raise ValueError('partial write')
    with pytest.raises(RuntimeError):
        async with storage_writes.fenced():raise AssertionError('read served')
    assert len(list((tmp_path/'.index-writes').glob('*.json')))==1
    seen=[]
    async def restore(data):seen.append(data)
    monkeypatch.setattr(storage_writes,'_restore',restore)
    await storage_writes.recover()
    assert len(seen)==1 and not list((tmp_path/'.index-writes').glob('*.json'))


@pytest.mark.anyio
async def test_cancelled_writer_joins_model_free_compensation(monkeypatch,tmp_path):
    monkeypatch.setattr(settings,'storage_root',str(tmp_path));kb,doc=str(uuid.uuid4()),str(uuid.uuid4());restored=asyncio.Event()
    async def snapshot(*args,**kwargs):return []
    async def restore(data):await asyncio.sleep(0.02);restored.set()
    monkeypatch.setattr(storage_writes,'_snapshot',snapshot);monkeypatch.setattr(storage_writes,'_restore',restore)
    entered=asyncio.Event()
    async def worker():
        async with storage_writes.replacement(kb,doc,['active']):entered.set();await asyncio.sleep(10)
    task=asyncio.create_task(worker());await entered.wait();task.cancel()
    with pytest.raises(asyncio.CancelledError):await task
    assert restored.is_set() and not list((tmp_path/'.index-writes').glob('*.json'))


def test_journal_cannot_restore_foreign_documents_or_special_collections():
    kb,doc=str(uuid.uuid4()),str(uuid.uuid4())
    data={'version':1,'id':str(uuid.uuid4()),'kb':kb,'doc':doc,'collections':{'active':[]},
        'parent_collection':'pai_kb_parents','note_collection':'pai_doc_notes','parents':[],'notes':[]}
    storage_writes._validate(data)
    data['collections']['active']=[{'id':str(uuid.uuid4()),'payload':{'knowledge_base_id':kb,'doc_id':str(uuid.uuid4())},'vector':[1.0]}]
    with pytest.raises(storage_writes.StorageRecoveryRequired):storage_writes._validate(data)
    data['collections']={'pai_kb_parents':[]}
    with pytest.raises(storage_writes.StorageRecoveryRequired):storage_writes._validate(data)


@pytest.mark.anyio
async def test_process_shared_rebuild_lock_refuses_another_replica_then_releases(monkeypatch, tmp_path):
    import subprocess, sys
    monkeypatch.setattr(settings, 'storage_root', str(tmp_path))
    code = """import asyncio, sys
from app.config import settings
from app.storage_writes import rebuild_guard, StorageRecoveryRequired
settings.storage_root = sys.argv[1]
async def run():
    try:
        async with rebuild_guard('desired', required=True): pass
    except StorageRecoveryRequired: raise SystemExit(23)
asyncio.run(run())
"""
    async with storage_writes.rebuild_guard('desired', required=True):
        blocked = await asyncio.to_thread(subprocess.run, [sys.executable, '-c', code, str(tmp_path)], capture_output=True)
        assert blocked.returncode == 23, blocked.stderr
    free = await asyncio.to_thread(subprocess.run, [sys.executable, '-c', code, str(tmp_path)], capture_output=True)
    assert free.returncode == 0, free.stderr


@pytest.mark.anyio
async def test_document_write_refuses_alias_changed_before_lock_without_snapshot_or_mutation(monkeypatch, tmp_path):
    from app import qdrant_store
    monkeypatch.setattr(settings, 'storage_root', str(tmp_path))
    async def current(*args, **kwargs):
        assert storage_writes._owner.get(), 'alias check must occur under the write fence'
        return 'published-new'
    async def unexpected(*args, **kwargs):
        raise AssertionError('stale writer took a snapshot or modified the corpus')
    monkeypatch.setattr(qdrant_store, 'alias_target', current)
    monkeypatch.setattr(storage_writes, '_snapshot', unexpected)
    with pytest.raises(storage_writes.StorageRecoveryRequired, match='active embedding index changed'):
        async with storage_writes.replacement(str(uuid.uuid4()), str(uuid.uuid4()), ['retired-old'], expected_active='retired-old'):
            raise AssertionError('stale writer reached corpus mutation')
    assert not list((tmp_path / '.index-writes').glob('*.json'))
