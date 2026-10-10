"""Versioned offline reference audit and explicit-format derivation. No model calls."""
import argparse
import ast
import calendar
from datetime import date,timedelta
import hashlib
import json
import re
from pathlib import Path

from cohort_report import public_cohort
from judge_review import digest,load,validate_dataset,write_new
from public_benchmark import INSTRUCTION

VERSION='fosnie-reference-audit-v1'
FORMAT_VERSION='explicit-answer-format-v1'
DIRECTIVES={
    'boolean':'Return exactly True or False, with that capitalisation. Output nothing else.',
    'choice':'Select the option answering the question. Return only its label, such as (A), not the option text or an explanation.',
    'words':'Return only the sorted words, separated by single spaces. Do not use commas, bullets, Markdown or an explanation.',
    'integer':'Return only the resulting integer, optionally beginning with a minus sign. Output nothing else.',
    'brackets':'Return only the missing closing brackets, separated by single spaces. Output nothing else.',
    'yes_no':'Return exactly Yes or No, with that capitalisation. Output nothing else.',
}


class Unsupported(ValueError):pass


def _ast_value(expression,kind):
    if not isinstance(expression,str) or len(expression)>8192:raise Unsupported('expression bound')
    try:tree=ast.parse(expression,mode='eval')
    except (SyntaxError,ValueError,RecursionError):raise Unsupported('expression syntax') from None
    if sum(1 for _ in ast.walk(tree))>512:raise Unsupported('expression node bound')
    def walk(node,depth=0):
        if depth>64:raise Unsupported('expression depth bound')
        child=lambda value:walk(value,depth+1)
        if isinstance(node,ast.Expression):return child(node.body)
        if isinstance(node,ast.Constant) and type(node.value) is (bool if kind=='boolean' else int):
            if kind=='integer' and abs(node.value)>10**6:raise Unsupported('integer bound')
            return node.value
        if kind=='boolean' and isinstance(node,ast.UnaryOp) and isinstance(node.op,ast.Not):return not child(node.operand)
        if kind=='boolean' and isinstance(node,ast.BoolOp) and isinstance(node.op,(ast.And,ast.Or)):
            values=[child(item) for item in node.values]
            return all(values) if isinstance(node.op,ast.And) else any(values)
        if kind=='integer' and isinstance(node,ast.UnaryOp) and isinstance(node.op,(ast.UAdd,ast.USub)):
            value=child(node.operand);return value if isinstance(node.op,ast.UAdd) else -value
        if kind=='integer' and isinstance(node,ast.BinOp) and isinstance(node.op,(ast.Add,ast.Sub,ast.Mult)):
            left,right=child(node.left),child(node.right)
            value=left+right if isinstance(node.op,ast.Add) else left-right if isinstance(node.op,ast.Sub) else left*right
            if abs(value)>10**12:raise Unsupported('arithmetic result bound')
            return value
        raise Unsupported('unsupported expression node')
    return walk(tree)


def original_question(case):
    value=case['conversation']
    if not value.endswith(INSTRUCTION):raise ValueError('unknown parent prompt contract')
    return value[:-len(INSTRUCTION)]


def options(question):
    found=re.findall(r'^\(([A-Z])\)[ \t]+([^\r\n]+)$',question,re.M)
    if len({label for label,_ in found})!=len(found):raise Unsupported('duplicate option label')
    return dict(found)


def _date_answer(question):
    lead=question.split('\nOptions:',1)[0]
    married=re.fullmatch(r'\w+ and \w+ married on ([A-Z][a-z]{2}) (\d{1,2}), (\d{4})\. It is their (\d{1,3})-year anniversary today\. What is the date one week from today in MM/DD/YYYY\?',lead)
    if married:
        month_text,day,year,anniversary=married.groups()
        months={name:i for i,name in enumerate(('Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'),1)}
        if month_text not in months or int(anniversary)>150:raise Unsupported('date bounds')
        try:result=date(int(year)+int(anniversary),months[month_text],int(day))+timedelta(days=7)
        except (ValueError,OverflowError):raise Unsupported('invalid or ambiguous anniversary') from None
        return result.strftime('%m/%d/%Y'),{'kind':'anniversary_plus_week','year':int(year),'anniversary':int(anniversary),'days_after':7}
    birthday=re.fullmatch(r'\w+ was born on the last day of Feb(?:urary|ruary) in (\d{4})\. Today is (?:her|his) (\d{1,3})-year-old birthday\. What is the date a month ago in MM/DD/YYYY\?',lead)
    if birthday:
        year,age=map(int,birthday.groups())
        if calendar.isleap(year) or age>150:raise Unsupported('ambiguous leap birthday or age bound')
        try:result=date(year+age,1,28)
        except ValueError:raise Unsupported('birthday date bound') from None
        return result.strftime('%m/%d/%Y'),{'kind':'february_birthday_minus_month','birth_year':year,'age':age,'birth_day':28}
    raise Unsupported('unsupported date grammar')


def solve(case):
    question=original_question(case);family=case['family_id']
    if family=='bbh/boolean_expressions':
        if not question.endswith(' is'):raise Unsupported('Boolean prompt grammar')
        value=str(_ast_value(question[:-3].strip(),'boolean'))
        return value,value,'boolean',{'kind':'restricted_boolean_ast'}
    if family=='bbh/multistep_arithmetic_two':
        if not question.rstrip().endswith('='):raise Unsupported('arithmetic prompt grammar')
        value=str(_ast_value(question.rstrip()[:-1].strip(),'integer'))
        return value,value,'integer',{'kind':'bounded_integer_ast'}
    if family=='bbh/word_sorting':
        match=re.fullmatch(r'Sort the following words alphabetically: List: ([a-zA-Z \t]+)',question)
        if not match:raise Unsupported('word grammar')
        words=match.group(1).split()
        if not 1<=len(words)<=200 or len(set(words))!=len(words):raise Unsupported('word count or duplicates')
        value=' '.join(sorted(words));return value,value,'words',{'kind':'ascii_lexicographic_sort','words':len(words)}
    if family=='bbh/dyck_languages':
        prefix='Complete the rest of the sequence, making sure that the parentheses are closed properly. Input: '
        if not question.startswith(prefix):raise Unsupported('bracket prompt grammar')
        sequence=question[len(prefix):];symbols=''.join(sequence.split());pairs={'(':')','[':']','{':'}','<':'>'};stack=[]
        if not 1<=len(symbols)<=4096 or any(ch not in '()[]{}<>' for ch in symbols):raise Unsupported('bracket symbols/bound')
        for ch in symbols:
            if ch in pairs:stack.append(pairs[ch])
            elif not stack or stack.pop()!=ch:raise Unsupported('invalid bracket prefix')
        if not stack:raise Unsupported('no nonempty completion')
        value=' '.join(reversed(stack));return value,value,'brackets',{'kind':'stack_completion','input_symbols':len(symbols)}
    if family=='bbh/navigate':
        prefix='If you follow these instructions, do you return to the starting point? '
        if not question.startswith(prefix):raise Unsupported('navigation prompt grammar')
        commands=question[len(prefix):].split('\nOptions:',1)[0].strip()
        fixed=commands.startswith('Always face forward. ')
        if fixed:commands=commands[len('Always face forward. '):]
        x=y=direction=0;vectors=((0,1),(1,0),(0,-1),(-1,0))
        segments=commands.split('. ')
        if len(segments)>256:raise Unsupported('navigation bound')
        for segment in segments:
            segment=segment.removesuffix('.')
            turn={'Turn left':-1,'Turn right':1,'Turn around':2}.get(segment)
            if turn is not None:
                if fixed:raise Unsupported('turn contradicts fixed orientation')
                direction=(direction+turn)%4;continue
            move=re.fullmatch(r'Take (\d{1,4}) steps?(?: (forward|backward|left|right))?',segment)
            if not move:raise Unsupported('unsupported navigation instruction')
            count=int(move.group(1));offset={None:0,'forward':0,'right':1,'backward':2,'left':-1}[move.group(2)]
            dx,dy=vectors[(direction+offset)%4];x+=count*dx;y+=count*dy
        value='Yes' if (x,y)==(0,0) else 'No'
        return value,value,'yes_no',{'kind':'bounded_navigation','final_position':[x,y]}
    if family=='bbh/date_understanding':
        value,proof=_date_answer(question);choices=options(question)
        labels=[label for label,text in choices.items() if text.strip()==value]
        if len(labels)!=1:raise Unsupported('computed date not unique in options')
        return '('+labels[0]+')',value,'choice',proof
    raise Unsupported('no deterministic verifier implemented')


def audit(dataset,bank,provenance):
    if bank.get('dataset_sha256')!=digest(dataset) or provenance.get('dataset_sha256')!=digest(dataset) or provenance.get('answer_bank_sha256')!=digest(bank):
        raise ValueError('parent reference binding mismatch')
    by_id={row['case_id']:row for row in bank['cases']}
    if len(by_id)!=len(bank['cases']) or set(by_id)!={c['id'] for c in dataset['cases']}:raise ValueError('parent case coverage')
    rows=[]
    for case in dataset['cases']:
        source=by_id[case['id']];row={'case_id':case['id'],'family_id':case['family_id'],'split':case['split'],
            'source_target_sha256':digest(source['target']),'source_input_sha256':source['original_input_sha256']}
        try:
            corrected,value,kind,proof=solve(case)
            row.update(status='verified' if corrected==source['target'] else 'correction_proposed',computed_target=corrected,
                       computed_value=value,format_kind=kind,proof=proof)
        except Unsupported as failure:row.update(status='unverified',reason=str(failure))
        rows.append(row)
    return {'schema_version':1,'audit_version':VERSION,'verifier_source_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            'parent_dataset_sha256':digest(dataset),'parent_answer_bank_sha256':digest(bank),'parent_provenance_sha256':digest(provenance),
            'quality_evidence':False,'human_reviewed':False,'model_calls':0,'network_calls':0,'rows':rows}


def derive(dataset,bank,provenance,checked,version):
    if not isinstance(version,str) or not re.fullmatch('[a-zA-Z0-9_.-]{1,64}',version) or version==dataset['dataset_id']:
        raise ValueError('distinct bounded derived version required')
    if checked!=audit(dataset,bank,provenance):raise ValueError('audit is stale, modified or foreign')
    cases={case['id']:case for case in dataset['cases']};source={row['case_id']:row for row in bank['cases']};prompts=[];answers=[];lineage=[]
    for row in checked['rows']:
        if row['status']=='unverified':continue
        case=cases[row['case_id']];kind=row['format_kind'];question=original_question(case)
        prompt={**case,'conversation':question+'\n\n'+DIRECTIVES[kind]};prompts.append(prompt)
        answers.append({**source[row['case_id']],'target':row['computed_target']})
        lineage.append({'case_id':case['id'],'source_target':source[case['id']]['target'],'derived_target':row['computed_target'],
                        'reference_changed':row['status']=='correction_proposed','format_kind':kind,'proof':row['proof']})
    if not prompts:raise ValueError('no deterministically checked cases')
    derived={'schema_version':1,'dataset_id':version,'source':{'kind':dataset['source']['kind'],
             'reference':'Derived from '+dataset['dataset_id']+'; deterministic audit '+digest(checked)},'cases':prompts}
    validate_dataset(derived)
    answer_bank={**bank,'dataset_sha256':digest(derived),'validator':'exact_trimmed_text_explicit_format_v1','cases':answers}
    manifest={'schema_version':1,'version':version,'parent_dataset_sha256':digest(dataset),'parent_answer_bank_sha256':digest(bank),
              'parent_provenance_sha256':digest(provenance),'audit_sha256':digest(checked),'audit_version':VERSION,'format_version':FORMAT_VERSION,
              'dataset_sha256':digest(derived),'answer_bank_sha256':digest(answer_bank),'source_lineage_sha256':digest(lineage),
              'case_count':len(prompts),'excluded_case_ids':[r['case_id'] for r in checked['rows'] if r['status']=='unverified'],
              'exposure_inherited':True,'fresh_holdout':False,'quality_evidence':False,'human_reviewed':False}
    return derived,answer_bank,manifest,lineage


def check(directory,parent):
    dataset,bank,provenance=public_cohort(parent);directory=Path(directory)
    checked=load(directory/'reference-audit.json');manifest=load(directory/'manifest.json')
    expected=derive(dataset,bank,provenance,checked,manifest['version'])
    actual=(load(directory/'prompts.json'),load(directory/'answer-bank.json'),manifest,load(directory/'source-lineage.json'))
    if actual!=expected:raise ValueError('derived source, prompt, answer or manifest mismatch')
    if (directory/'LICENSE-BBH.txt').read_bytes()!=(Path(parent)/'LICENSE-BBH.txt').read_bytes():raise ValueError('source licence mismatch')
    return expected


def main():
    parser=argparse.ArgumentParser(description=__doc__);commands=parser.add_subparsers(dest='command',required=True)
    create=commands.add_parser('create');create.add_argument('--parent',required=True);create.add_argument('--output',required=True);create.add_argument('--version',required=True)
    verify=commands.add_parser('check');verify.add_argument('--parent',required=True);verify.add_argument('--derived',required=True)
    args=parser.parse_args()
    if args.command=='check':result=check(args.derived,args.parent)
    else:
        parent=Path(args.parent).resolve();output=Path(args.output).resolve()
        if output==parent or output.is_relative_to(parent):raise ValueError('derived output cannot modify parent directory')
        dataset,bank,provenance=public_cohort(parent);checked=audit(dataset,bank,provenance);result=derive(dataset,bank,provenance,checked,args.version)
        output.mkdir(exist_ok=False)
        for name,value in [('prompts',result[0]),('answer-bank',result[1]),('manifest',result[2]),('source-lineage',result[3]),('reference-audit',checked)]:write_new(output/(name+'.json'),value)
        with (output/'LICENSE-BBH.txt').open('xb') as licence:licence.write((parent/'LICENSE-BBH.txt').read_bytes())
    print(json.dumps({'status':'checked','cases':len(result[0]['cases']),'new_holdout':False,'model_calls':0,'network_calls':0}))


if __name__=='__main__':main()
