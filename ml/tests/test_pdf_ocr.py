# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio,io,json,threading
from uuid import uuid4
import httpx,pytest
from pypdf import PdfWriter,PdfReader
from PIL import Image
from app import extract,http_client,pdf_ocr,rag_ctx
from app.inference_budget import BudgetExhausted
from app.inference_fixed import FixedTarget
from test_inference_target import clean_context,anyio_backend,target


def envelope(calls=2,mode='page_images'):
    return dict(schema_version=1,decision_id=str(uuid4()),purpose='ocr',assignments={},
        fixed_assignments={'ocr':dict(schema_version=1,provider_id=str(uuid4()),provider_revision=1,
            profile_revision=1,role='ocr',model='fixture-ocr',base_url='https://ocr.test/v1',api_key='',ocr_pdf_mode=mode)},
        fixed_budget=dict(id=str(uuid4()),timeout_ms=5000,allowances={'ocr':dict(max_calls=calls,input_bytes=8*1024*1024,output_bytes=8192)}))


def scanned(tmp_path,count=2,width=80,height=30):
    image=Image.new('RGB',(width,height),'white');buffer=io.BytesIO()
    image.save(buffer,format='PDF',save_all=True,append_images=[image]*(count-1),resolution=144)
    image.close();p=tmp_path/'scan.pdf';p.write_bytes(buffer.getvalue());return p


@pytest.mark.anyio
async def test_actual_raster_sends_each_page_as_pinned_png_and_preserves_page_numbers(monkeypatch,tmp_path):
    path=scanned(tmp_path);seen=[]
    def reply(request):
        body=json.loads(request.content);seen.append(body)
        assert body['model']=='fixture-ocr'
        assert body['messages'][0]['content'][1]['image_url']['url'].startswith('data:image/png;base64,')
        return httpx.Response(200,json={'choices':[{'message':{'content':'Page '+str(len(seen))}}]})
    rag_ctx.set_overrides({'routing':envelope(),'ocr_pdf_mode':'document','ocr_base_url':'https://wrong.test'})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        pages=await extract.extract_pages_ocr(str(path),'application/pdf')
    assert pages==[(1,'Page 1'),(2,'Page 2')] and len(seen)==2
    assert rag_ctx.cfg('_routing_fixed_budget',None).counts=={'ocr':2}


@pytest.mark.anyio
async def test_mixed_pdf_keeps_native_page_and_ocr_only_the_scanned_page(monkeypatch,tmp_path):
    path=scanned(tmp_path)
    monkeypatch.setattr(extract,'extract_pages',lambda *args:[(1,'Native first page text remains original'),(2,'')])
    rag_ctx.set_overrides({'routing':envelope(calls=1)})
    seen=[]
    def reply(request):
        seen.append(request);return httpx.Response(200,json={'choices':[{'message':{'content':'Scanned second page'}}]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        result=await extract.extract_pages_ocr(str(path))
    assert result==[(1,'Native first page text remains original'),(2,'Scanned second page')] and len(seen)==1


@pytest.mark.anyio
async def test_document_profile_overrides_page_image_environment_and_keeps_legacy_single_call(monkeypatch,tmp_path):
    path=scanned(tmp_path);rag_ctx.set_overrides({'routing':envelope(calls=1,mode='document'),'ocr_pdf_mode':'page_images'})
    seen=[]
    def reply(request):
        body=json.loads(request.content);seen.append(body)
        assert body['messages'][0]['content'][1]['image_url']['url'].startswith('data:application/pdf;base64,')
        return httpx.Response(200,json={'choices':[{'message':{'content':'Document transcription'}}]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        assert await extract.extract_pages_ocr(str(path))==[(1,'Document transcription')]
    assert len(seen)==1


@pytest.mark.anyio
@pytest.mark.parametrize('pages,calls',[(9,9),(2,1)])
async def test_page_caps_and_remaining_grants_refuse_before_render_or_model_calls(monkeypatch,tmp_path,pages,calls):
    path=scanned(tmp_path,count=pages);rag_ctx.set_overrides({'routing':envelope(calls=calls)})
    def unexpected(*args):raise AssertionError('render started before capacity check')
    monkeypatch.setattr(pdf_ocr,'_render_pages',unexpected)
    with pytest.raises(BudgetExhausted):await extract.extract_pages_ocr(str(path))
    assert not rag_ctx.cfg('_routing_fixed_budget',None).counts


@pytest.mark.anyio
async def test_native_pdf_needs_no_ocr_assignment(monkeypatch,tmp_path):
    path=scanned(tmp_path,count=1)
    monkeypatch.setattr(extract,'extract_pages',lambda *args:[(1,'Native PDF text needs no model call')])
    rag_ctx.set_overrides({'routing':{'schema_version':1,'decision_id':str(uuid4()),'purpose':'utility_summary','assignments':{'utility_summary':target()}}})
    assert await extract.extract_pages_ocr(str(path))==[(1,'Native PDF text needs no model call')]


@pytest.mark.anyio
async def test_cancelled_render_does_not_start_late_ocr(monkeypatch,tmp_path):
    path=scanned(tmp_path,count=1);started=threading.Event();release=threading.Event();finished=threading.Event()
    rag_ctx.set_overrides({'routing':envelope(calls=1)})
    def render(*args):
        started.set()
        try:release.wait(2);return [(0,b'fixture')]
        finally:finished.set()
    monkeypatch.setattr(pdf_ocr,'_render_pages',render)
    async def unexpected(*args):raise AssertionError('late OCR after cancellation')
    monkeypatch.setattr(pdf_ocr.ocr,'ocr_bytes',unexpected)
    task=asyncio.create_task(extract.extract_pages_ocr(str(path)))
    try:
        for _ in range(100):
            if started.is_set():break
            await asyncio.sleep(0.01)
        assert started.is_set();task.cancel()
        with pytest.raises(asyncio.CancelledError):await task
    finally:
        release.set()
        for _ in range(100):
            if finished.is_set():break
            await asyncio.sleep(0.01)
    assert finished.is_set() and not rag_ctx.cfg('_routing_fixed_budget',None).counts


def test_oversized_page_geometry_is_refused_before_bitmap_allocation(tmp_path):
    writer=PdfWriter();writer.add_blank_page(width=10000,height=10000);path=tmp_path/'large.pdf'
    with path.open('wb') as f:writer.write(f)
    with pytest.raises(BudgetExhausted):pdf_ocr._render_pages(str(path),[0])


def test_pdf_input_mode_cannot_be_injected_into_an_embedding_target():
    value=envelope()['fixed_assignments']['ocr'];value['role']='embed'
    with pytest.raises(ValueError):FixedTarget.model_validate(value)
