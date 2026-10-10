# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Model-free recovery of bounded document writes under a shared storage lock.

The lock/journals live with shared document storage, so all Fosnie ML replicas
using this directory fence their Qdrant operations. External Qdrant clients are
outside this protocol. Journals contain corpus data, never inference credentials.
"""
import asyncio,contextlib,contextvars,functools,inspect,json,os,re,time,uuid
from pathlib import Path
from .config import settings

MAX_BYTES=128*1024*1024
MAX_POINTS=8192
_owner=contextvars.ContextVar('index_storage_owner',default=False)


class StorageRecoveryRequired(RuntimeError):pass


def root():
    if not settings.storage_root.strip():return None
    base=Path(settings.storage_root).resolve();folder=base/'.index-writes'
    folder.mkdir(mode=0o700,exist_ok=True)
    if folder.resolve().parent!=base:raise StorageRecoveryRequired('index journal confinement')
    return folder


class _Lock:
    def __init__(self,path):
        if path.is_symlink():raise StorageRecoveryRequired('storage lock confinement')
        self.file=path.open('a+b')
    def acquire(self):
        self.file.seek(0,2)
        if self.file.tell()==0:self.file.write(b'0');self.file.flush()
        self.file.seek(0)
        try:
            if os.name=='nt':
                import msvcrt;msvcrt.locking(self.file.fileno(),msvcrt.LK_NBLCK,1)
            else:
                import fcntl;fcntl.flock(self.file.fileno(),fcntl.LOCK_EX|fcntl.LOCK_NB)
            return True
        except OSError:return False
    def close(self):self.file.close()  # OS releases locks even after process death.


@contextlib.asynccontextmanager
async def fenced():
    if _owner.get() or root() is None:
        yield;return
    lock=_Lock(root()/'storage.lock');started=time.monotonic();token=None
    try:
        while not lock.acquire():
            if time.monotonic()-started>=5:raise StorageRecoveryRequired('index storage is busy')
            await asyncio.sleep(0.025)
        token=_owner.set(True)
        await _recover_locked()
        yield
    finally:
        if token is not None:_owner.reset(token)
        lock.close()


@contextlib.asynccontextmanager
async def rebuild_guard(collection, *, required=False):
    """One builder/publication per physical target across shared-root replicas.

    This separate OS lock spans inference without blocking ordinary corpus reads
    or document replacements. Contention fails before reset or model dispatch.
    Process death releases it; an explicit new job can then rebuild the target.
    """
    folder = root()
    if folder is None:
        if required:
            raise StorageRecoveryRequired('shared storage root required for rebuild')
        yield
        return
    if not isinstance(collection, str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,512}', collection):
        raise StorageRecoveryRequired('rebuild target identity')
    import hashlib
    locks = folder / 'rebuild-locks'
    locks.mkdir(mode=0o700, exist_ok=True)
    if locks.resolve().parent != folder.resolve():
        raise StorageRecoveryRequired('rebuild lock confinement')
    lock = _Lock(locks / (hashlib.sha256(collection.encode()).hexdigest() + '.lock'))
    try:
        if not lock.acquire():
            raise StorageRecoveryRequired('embedding rebuild target is busy')
        yield
    finally:
        lock.close()


def guarded(fn):
    if inspect.isasyncgenfunction(fn):
        @functools.wraps(fn)
        async def generator(*args,**kwargs):
            source=fn(*args,**kwargs)
            try:
                while True:
                    try:
                        async with fenced():item=await anext(source)
                    except StopAsyncIteration:return
                    yield item
            finally:await source.aclose()
        return generator
    @functools.wraps(fn)
    async def call(*args,**kwargs):
        async with fenced():return await fn(*args,**kwargs)
    return call


def _filter(kb,doc):
    from qdrant_client import models
    return models.Filter(must=[models.FieldCondition(key='knowledge_base_id',match=models.MatchValue(value=kb)),
        models.FieldCondition(key='doc_id',match=models.MatchValue(value=doc))])


async def _snapshot(collection,kb,doc,note=False):
    from . import qdrant_store
    c=qdrant_store.client()
    if not await c.collection_exists(collection):return []
    if note:
        points=await c.retrieve(collection,ids=[doc],with_payload=True,with_vectors=True)
    else:
        points=[];offset=None
        while True:
            batch,offset=await c.scroll(collection,scroll_filter=_filter(kb,doc),offset=offset,limit=128,with_payload=True,with_vectors=True)
            points.extend(batch)
            if len(points)>MAX_POINTS:raise StorageRecoveryRequired('document snapshot point cap')
            if offset is None:break
    return [p.model_dump(mode='json',include={'id','payload','vector'}) for p in points]


def _validate(data):
    from . import qdrant_store
    if set(data)!={'version','id','kb','doc','collections','parents','notes','note_collection','parent_collection'} or data['version']!=1:
        raise StorageRecoveryRequired('unsupported storage journal')
    uuid.UUID(data['id']);uuid.UUID(data['kb']);uuid.UUID(data['doc'])
    if data['parent_collection']!=qdrant_store.PARENTS or data['note_collection']!=qdrant_store.NOTES:
        raise StorageRecoveryRequired('journal collection identity')
    if not isinstance(data['collections'],dict) or not 1<=len(data['collections'])<=2:raise StorageRecoveryRequired('journal targets')
    for name in data['collections']:
        if not re.fullmatch(r'[A-Za-z0-9_.-]{1,512}',name) or name in (qdrant_store.PARENTS,qdrant_store.NOTES,qdrant_store.TOC):
            raise StorageRecoveryRequired('journal target identity')
    for name,points in [*data['collections'].items(),(data['parent_collection'],data['parents']),(data['note_collection'],data['notes'])]:
        if not isinstance(points,list) or len(points)>MAX_POINTS:raise StorageRecoveryRequired('journal point cap')
        for point in points:
            if set(point)!={'id','payload','vector'}:raise StorageRecoveryRequired('journal point shape')
            payload=point['payload'] or {}
            if name==data['note_collection']:
                if str(point['id'])!=data['doc']:raise StorageRecoveryRequired('journal note scope')
            elif payload.get('knowledge_base_id')!=data['kb'] or payload.get('doc_id')!=data['doc']:
                raise StorageRecoveryRequired('journal document scope')


async def _restore(data):
    from . import qdrant_store
    from qdrant_client import models
    _validate(data);c=qdrant_store.client()
    groups=[*data['collections'].items(),(data['parent_collection'],data['parents']),(data['note_collection'],data['notes'])]
    for name,points in groups:
        if not await c.collection_exists(name):
            if points:raise StorageRecoveryRequired('snapshot collection is unavailable')
            continue
        selector=models.PointIdsList(points=[data['doc']]) if name==data['note_collection'] else models.FilterSelector(filter=_filter(data['kb'],data['doc']))
        await c.delete(name,points_selector=selector,wait=True)
        restored=[models.PointStruct.model_validate(p) for p in points]
        for start in range(0,len(restored),128):await c.upsert(name,points=restored[start:start+128],wait=True)


def _save(folder,data):
    raw=json.dumps(data,separators=(',',':'),allow_nan=False).encode()
    if len(raw)>MAX_BYTES:raise StorageRecoveryRequired('document snapshot byte cap')
    path=folder/(data['id']+'.json');temp=folder/(data['id']+'.new')
    with temp.open('xb') as file:
        file.write(raw);file.flush();os.fsync(file.fileno())
    os.replace(temp,path)
    return path


async def _recover_locked():
    folder=root()
    if folder is None:return
    paths=list(folder.glob('*.json'))
    if len(paths)>128:raise StorageRecoveryRequired('storage recovery journal cap')
    for path in paths:
        if not re.fullmatch(r'[0-9a-f-]{36}\.json',path.name) or path.is_symlink() or path.stat().st_size>MAX_BYTES:
            raise StorageRecoveryRequired('storage journal identity/bound')
        data=json.loads(path.read_text(encoding='utf-8'))
        if path.stem!=data.get('id'):raise StorageRecoveryRequired('storage journal identity')
        await _restore(data)
        path.unlink()


async def recover():
    async with fenced():pass


@contextlib.asynccontextmanager
async def replacement(kb,doc,collections,*,expected_active=None):
    from . import qdrant_store,rag_ctx
    enabled=rag_ctx.cfg('storage_recovery',bool(settings.storage_root.strip()))
    if not enabled:
        yield;return  # existing unconstrained development mode only
    if root() is None:raise StorageRecoveryRequired('configured shared storage root is required')
    kb=str(uuid.UUID(kb));doc=str(uuid.UUID(doc));names=list(dict.fromkeys(collections))
    async with fenced():
        if expected_active is not None and await qdrant_store.alias_target(qdrant_store.COLLECTION,strict=True)!=expected_active:
            raise StorageRecoveryRequired('active embedding index changed before document replacement')
        data={'version':1,'id':str(uuid.uuid4()),'kb':kb,'doc':doc,'collections':{},
            'parent_collection':qdrant_store.PARENTS,'note_collection':qdrant_store.NOTES,
            'parents':await _snapshot(qdrant_store.PARENTS,kb,doc),'notes':await _snapshot(qdrant_store.NOTES,kb,doc,True)}
        for name in names:data['collections'][name]=await _snapshot(name,kb,doc)
        _validate(data);path=_save(root(),data)
        try:
            yield
        except BaseException:
            # Shield model-free compensation from the cancelled request. Wait
            # for it before releasing the process-shared lock to a newer write.
            task=asyncio.create_task(_restore(data))
            try:await asyncio.shield(task)
            except asyncio.CancelledError:
                await task
            except Exception as error:
                raise StorageRecoveryRequired('document write recovery remains pending') from error
            path.unlink();raise
        else:
            # A process crash before this marker deletion rolls back on the next
            # fenced operation. No inference is rerun and no grant is replenished.
            path.unlink()
