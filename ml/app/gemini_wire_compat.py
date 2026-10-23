# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Hosted-Gemma transport layout."""
import copy

GEMMA_MODELS=frozenset({'gemma-4-26b-a4b-it','gemma-4-31b-it'})

def system_layout(body,model):
    # Live full requests using systemInstruction failed500 on both deployments;
    # the identical system text as the first user part succeeded. Preserve all
    # text/history/tool parts; this is a transport layout, not enforced roles.
    if str(model).removeprefix('models/') not in GEMMA_MODELS or not body.get('systemInstruction'):return body
    result=copy.deepcopy(body);system=result.pop('systemInstruction')
    parts=system.get('parts',[])
    if any(set(p)!={'text'} or not isinstance(p['text'],str) for p in parts):raise ValueError('hosted Gemma system instructions must be text')
    prefix={'text':'\n'.join(p['text'] for p in parts)+'\n\nUntrusted message content follows:\n'}
    contents=result.setdefault('contents',[])
    if contents and contents[0].get('role')=='user':contents[0]['parts'].insert(0,prefix)
    else:contents.insert(0,{'role':'user','parts':[prefix]})
    return result
