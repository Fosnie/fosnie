"""Revision-bound public text pilot; candidate families require independent review."""
import argparse
import ast
from collections import defaultdict
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import re
import unicodedata

from judge_review import digest, encoded, freeze, load, prepare_files, prompt_digest, validate_dataset, write_new

EXPECTED = {'gsm8k': '3101c7d5072418e28b9008a6636bde82a006892c',
            'squad': 'eee5fdbf62f8613a7812b03419e6b29617b74fd1',
            'mera': '73cf223e40f1ef51ba74a048b584874a35c3d88b'}
VERSION = 'fosnie-auto-pilot-public-v1'


def arithmetic(text):
    if not isinstance(text, str) or len(text)>256 or not re.fullmatch(r'[\d.()+*/ \-]+', text):raise ValueError('bounded arithmetic only')
    tree=ast.parse(text, mode='eval')
    if sum(1 for _ in ast.walk(tree))>128:raise ValueError('arithmetic node bound')
    def value(node, depth=0):
        if depth>20:raise ValueError('arithmetic depth bound')
        if isinstance(node, ast.Constant) and type(node.value) in (int,float):result=Fraction(str(node.value))
        elif isinstance(node, ast.UnaryOp) and isinstance(node.op,(ast.UAdd,ast.USub)):
            result=value(node.operand,depth+1)*(1 if isinstance(node.op,ast.UAdd) else -1)
        elif isinstance(node,ast.BinOp) and isinstance(node.op,(ast.Add,ast.Sub,ast.Mult,ast.Div)):
            left=value(node.left,depth+1);right=value(node.right,depth+1)
            if isinstance(node.op,ast.Add):result=left+right
            elif isinstance(node.op,ast.Sub):result=left-right
            elif isinstance(node.op,ast.Mult):result=left*right
            else:result=left/right
        else:raise ValueError('unsupported arithmetic syntax')
        if abs(result.numerator)>10**12 or result.denominator>10**12:raise ValueError('arithmetic magnitude bound')
        return result
    return value(tree.body)


def source_documents(directory):
    directory=Path(directory).resolve();manifest=load(directory/'sources.json');documents={};sources={}
    if manifest.get('schema_version')!=1 or not isinstance(manifest.get('sources'),list):raise ValueError('source manifest')
    for row in manifest['sources']:
        if row['name'] not in EXPECTED or row['revision']!=EXPECTED[row['name']]:raise ValueError('immutable source revision')
        path=(directory/row['local_path']).resolve()
        if not path.is_relative_to(directory):raise ValueError('source path escape')
        with path.open('rb') as stream:raw=stream.read(16000001)
        if len(raw)>16000000 or len(raw)!=row['bytes'] or hashlib.sha256(raw).hexdigest()!=row['sha256']:raise ValueError('source hash/size binding')
        key=(row['name'],Path(row['path']).name)
        if key in documents:raise ValueError('duplicate source')
        documents[key]=raw;sources[key]=row
    required={('gsm8k','test.jsonl'),('gsm8k','LICENSE'),('gsm8k','README.md'),('squad','dev-v1.1.json'),('squad','README.md'),('mera','train.jsonl'),('mera','README.md')}
    if set(documents)!=required:raise ValueError('source file coverage')
    return documents,sources,manifest


def norm(text):return ' '.join(unicodedata.normalize('NFC',text).casefold().split())


def candidates(documents,sources):
    rows=[]
    def add(source,identity,family,language,conversation,answers,proof,original):
        if len(conversation.encode())>5000 or len(conversation)>12000:return
        rows.append({'source':source,'source_id':identity,'group':family,'language':language,'conversation':conversation,
                     'answers':sorted(set(answers)),'proof':proof,'original_input_sha256':digest(original)})
    for index,line in enumerate(documents[('gsm8k','test.jsonl')].decode('utf-8').splitlines()):
        row=json.loads(line);question=row['question'];solution=row['answer']
        final=re.search(r'####\s*(-?[\d,]+)\s*$',solution);annotations=re.findall(r'<<([^<>]+)=([^<>]+)>>',solution)
        if not final or not 1<=len(annotations)<=8:continue
        try:
            target=str(int(final[1].replace(',','')))
            for expression,result in annotations:
                if arithmetic(expression.replace(',',''))!=arithmetic(result.replace(',','')):raise ValueError('published calculation mismatch')
            if arithmetic(annotations[-1][1].replace(',',''))!=Fraction(target):continue
        except (ValueError,ZeroDivisionError,SyntaxError,OverflowError):continue
        skeleton=';'.join(re.sub(r'\d+(?:\.\d+)?','N',expression.replace(' ','').replace(',','')) for expression,_ in annotations)
        add('gsm8k',str(index),digest(skeleton),'en',question+'\n\nReturn only the final integer, without units or explanation.',[target],
            {'kind':'published_calculations_checked','annotations':len(annotations),'skeleton_sha256':digest(skeleton),
             'semantic_solution_verified':False},question)
    squad=json.loads(documents[('squad','dev-v1.1.json')])
    for article in squad['data']:
        for pindex,paragraph in enumerate(article['paragraphs']):
            context=paragraph['context']
            if len(context)>2200:continue
            for question in paragraph['qas']:
                answers=question['answers']
                if not answers or any(not isinstance(a['text'],str) or not 1<=len(a['text'])<=128 or context[a['answer_start']:a['answer_start']+len(a['text'])]!=a['text'] for a in answers):continue
                prompt='Read the supplied passage and answer the question using only it. Return only the exact answer span.\n\nPassage:\n'+context+'\n\nQuestion: '+question['question']
                add('squad',question['id'],digest(norm(article['title'])),'en',prompt,[a['text'] for a in answers],
                    {'kind':'all_published_spans_verified','article':article['title'],'paragraph_index':pindex,'retrieval_exercised':False},question['question'])
    for index,line in enumerate(documents[('mera','train.jsonl')].decode('utf-8').splitlines()):
        row=json.loads(line);inputs=row['inputs'];target=row['outputs']
        if set(inputs)!={'question','option_a','option_b','option_c','option_d'} or target not in ('A','B','C','D'):raise ValueError('MERA supported schema')
        question=inputs['question'];stem=re.sub(r'\d+(?:[.,]\d+)?','N',norm(question))
        prompt=question+'\n'+'\n'.join(label+'. '+inputs['option_'+label.lower()] for label in ('A','B','C','D'))+'\n\nОтветьте только одной буквой: A, B, C или D. Без объяснений.'
        add('mera',str(row['meta']['id']),digest(stem),'ru',prompt,[target],
            {'kind':'published_human_validated_translation_label','reference_computed':False,'semantic_family_reviewed':False},inputs)
    return rows


def convert(documents,sources,source_manifest,calibration_groups=5,test_groups=20):
    cases=[];bank=[];provenance=[]
    for source in ('gsm8k','squad','mera'):
        grouped=defaultdict(list)
        for row in candidates(documents,sources):
            if row['source']==source:grouped[row['group']].append(row)
        groups=sorted(grouped,key=lambda identity:digest([VERSION,source,identity]))
        if len(groups)<calibration_groups+test_groups:raise ValueError('insufficient source groups')
        for position,group in enumerate(groups[:calibration_groups+test_groups]):
            row=min(grouped[group],key=lambda r:digest([r['source_id'],r['original_input_sha256']]))
            identity=source+'/'+row['source_id'];family=source+'/'+group
            split='calibration' if position<calibration_groups else 'test'
            cases.append({'id':identity,'family_id':family,'split':split,'language':row['language'],'conversation':row['conversation']})
            data_source=sources[(source,'test.jsonl' if source=='gsm8k' else 'dev-v1.1.json' if source=='squad' else 'train.jsonl')]
            bank.append({'case_id':identity,'family_id':family,'split':split,'acceptable_answers':row['answers'],'validator':'strict_trimmed_one_of_v1',
                         'reference_proof':row['proof'],'source_url':data_source['url'],'source_sha256':data_source['sha256'],
                         'original_input_sha256':row['original_input_sha256']})
            provenance.append({'case_id':identity,'source':source,'source_id':row['source_id'],'source_group':group,
                               'grouping':'calculation_skeleton' if source=='gsm8k' else 'article' if source=='squad' else 'normalised_question_stem',
                               'family_independence_reviewed':False,'prompt_wrapper_authorship':'Fosnie AI, original question/context unchanged'})
    dataset={'schema_version':1,'dataset_id':VERSION,'source':{'kind':'independent','reference':'Revision-pinned human-written GSM8K/SQuAD and human-validated MERA; Fosnie formatting wrappers, candidate source groups not yet independently reviewed.'},'cases':cases}
    validate_dataset(dataset)
    answer_bank={'schema_version':1,'dataset_sha256':digest(dataset),'cases':bank,'quality_evidence':False}
    manifest={'schema_version':1,'dataset_sha256':digest(dataset),'answer_bank_sha256':digest(answer_bank),
              'source_manifest_sha256':digest(source_manifest),'provenance_sha256':digest(provenance),
              'case_count':len(cases),'calibration_source_groups':calibration_groups*3,'test_source_groups':test_groups*3,
              'model_outputs_used_for_selection':False,'known_pilot_exposure':False,'model_training_exposure_unknown':True,
              'family_independence_reviewed':False,'tier_labels_reviewed':False,'quality_evidence':False,
              'p12_eligible':False,'scope':'stateless_text; supplied_context_is_not_retrieval'}
    return dataset,answer_bank,manifest,provenance


def check(directory,sources_directory):
    docs,sources,source_manifest=source_documents(sources_directory);expected=convert(docs,sources,source_manifest)
    actual=tuple(load(Path(directory)/(name+'.json')) for name in ('dataset','answer-bank','source-manifest','provenance'))
    if expected!=actual:raise ValueError('derived pilot package binding mismatch')
    return expected


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('command',choices=('create','check'))
    parser.add_argument('--sources',required=True,type=Path);parser.add_argument('--output',required=True,type=Path)
    args=parser.parse_args()
    if args.command=='check':result=check(args.output,args.sources)
    else:
        if args.output.resolve().is_relative_to(args.sources.resolve()):raise ValueError('output cannot modify sources')
        docs,sources,manifest=source_documents(args.sources);result=convert(docs,sources,manifest)
        args.output.mkdir(exist_ok=False)
        for name,value in zip(('dataset','answer-bank','source-manifest','provenance'),result):write_new(args.output/(name+'.json'),value)
        write_new(args.output/'upstream-sources.json',manifest)
        with (args.output/'LICENSE-GSM8K.txt').open('xb') as stream:stream.write(docs[('gsm8k','LICENSE')])
        notices='GSM8K: '+sources[('gsm8k','LICENSE')]['url']+'\nMERA dataset card: '+sources[('mera','README.md')]['url']+' (MIT; automatically translated and human validated)\nSQuAD data: CC BY-SA4.0, https://rajpurkar.github.io/SQuAD-explorer/ ; Wikipedia context attribution retained in reference proof/article names.\nDerived format wrappers by Fosnie; references and raw sources retained.\n'
        with (args.output/'ATTRIBUTION.txt').open('x',encoding='utf-8') as stream:stream.write(notices)
        # Blind tier/requirement review remains blank. No machine label becomes human evidence.
        prepare_files(args.output/'dataset.json',args.output/'blind-review')
    print(json.dumps({'status':'checked','cases':len(result[0]['cases']),'test_source_groups':result[2]['test_source_groups'],
                      'family_independence_reviewed':False,'model_calls':0,'quality_evidence':False}))


if __name__=='__main__':main()
