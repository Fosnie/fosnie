"""Answer-shape hints for diagnosis only; the strict reference score never changes."""
import hashlib
import json
import re


def diagnose(content,reference,question):
    if not isinstance(content,str) or len(content.encode('utf-8'))>1024*1024 or not isinstance(reference,str) or not isinstance(question,str):
        raise ValueError('bounded text diagnostic required')
    text=content.strip();expected=reference.strip()
    result={'answer_sha256':hashlib.sha256(content.encode()).hexdigest(),'chars':len(content),
        'strict_reference_match':text==expected,'casefold_match':text.casefold()==expected.casefold(),
        'whitespace_sequence_match':' '.join(text.split())==' '.join(expected.split()),
        'markdown_fence':text.startswith('```'),'literal_think_marker':'<think>' in text or '</think>' in text,
        'known_prefix_reference_match':False,'fenced_reference_match':False,'json_string_reference_match':False,
        'option_text_reference_match':False,'other_enumerated_value_match':False,
        'diagnostic_only':True,'score_rewritten':False}
    for prefix in ('Answer:','The answer is:','Final answer:'):
        if text.startswith(prefix) and text[len(prefix):].strip()==expected:
            result['known_prefix_reference_match']=True
    fenced=re.fullmatch(r'```[^\n]*\n(.*?)\n```',text,re.S)
    if fenced:result['fenced_reference_match']=fenced.group(1).strip()==expected
    try:
        parsed=json.loads(text)
        result['json_string_reference_match']=isinstance(parsed,str) and parsed==expected
    except (ValueError,TypeError):pass
    options=dict(re.findall(r'^\(([A-Z])\)\s+(.+)$',question,re.M))
    choice=re.fullmatch(r'\(([A-Z])\)',expected)
    if choice and choice.group(1) in options:
        option=options[choice.group(1)].strip()
        result['option_text_reference_match']=text==option or text=='**'+option+'**'
        result['other_enumerated_value_match']=any(text==value.strip() for key,value in options.items() if key!=choice.group(1))
    if expected in ('True','False'):
        result['other_enumerated_value_match']=text==('False' if expected=='True' else 'True')
    return result
