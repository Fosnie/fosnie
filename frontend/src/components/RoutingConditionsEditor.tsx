// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useId, useState, cloneElement, type ReactNode, type ReactElement } from "react";
import { tasks,purposes,emptyConditions,type Conditions } from "@/api/routingSettings";
const human=(s:string)=>s.replaceAll("_"," ");
const list=(s:string)=>[...new Set(s.split(",").map(v=>v.trim()).filter(Boolean))];
function Field({label,children,hint}:{label:string;children:ReactNode;hint?:string}) {
  const id=useId();
  return <div className="routing-field"><label htmlFor={id}>{label}</label>
    {cloneElement(children as ReactElement<{id:string;"aria-describedby"?:string}>,{id,"aria-describedby":hint ? `${id}-hint` : undefined})}
    {hint && <small id={`${id}-hint`}>{hint}</small>}</div>;
}
function Choice<T extends string>({label,value,options,onChange,disabled=false}:{label:string;value:T;options:readonly T[];onChange:(v:T)=>void;disabled?:boolean}) {
  return <Field label={label}><select value={value} onChange={e=>onChange(e.target.value as T)} disabled={disabled}>{options.map(v=><option key={v} value={v}>{human(v)}</option>)}</select></Field>;
}
function NumberField({label,value,onChange,max=Number.MAX_SAFE_INTEGER,min=0,placeholder="Unknown / unlimited"}:{label:string;value:number|null;onChange:(v:number|null)=>void;max?:number;min?:number;placeholder?:string}) {
  return <Field label={label}><input type="number" min={min} max={max} step="1" value={value ?? ""} placeholder={placeholder} onChange={e=>onChange(e.target.value==="" ? null : Number(e.target.value))}/></Field>;
}
function Toggle({label,value,onChange}:{label:string;value:boolean;onChange:(v:boolean)=>void}) {
  return <label className="routing-check"><input type="checkbox" checked={value} onChange={e=>onChange(e.target.checked)}/><span>{label}</span></label>;
}
function ListField({label,values,onChange,hint,maxLength}:{label:string;values:string[];onChange:(v:string[])=>void;hint?:string;maxLength?:number}) {
  const [raw,setRaw]=useState(values.join(", "));
  return <Field label={label} hint={hint}><input maxLength={maxLength} value={raw} onChange={e=>{setRaw(e.target.value);onChange(list(e.target.value));}}/></Field>;
}
function Multiple<T extends string>({label,values,options,onChange}:{label:string;values:T[];options:readonly T[];onChange:(v:T[])=>void}) {
  return <div className="routing-field"><span>{label} <small>(none = any)</small></span><div className="routing-checks">{options.map(v=><Toggle key={v} label={human(v)} value={values.includes(v)} onChange={on=>onChange(on ? [...values,v] : values.filter(x=>x!==v))}/>)}</div></div>;
}
export function ConditionsEditor({value:c,onChange,depth=0}:{value:Conditions;onChange:(c:Conditions)=>void;depth?:number}) {
  const patch=(p:Partial<Conditions>)=>onChange({...c,...p});
  return <div className="routing-rule"><div className="routing-grid">
    <NumberField label="Minimum input tokens" value={c.min_input_tokens} max={4294967295} onChange={v=>patch({min_input_tokens:v})}/>
    <NumberField label="Maximum input tokens" value={c.max_input_tokens} max={4294967295} onChange={v=>patch({max_input_tokens:v})}/>
    <ListField label="Languages" hint="Codes such as en, ru or en-GB; blank = any." values={c.languages} onChange={v=>patch({languages:v})}/>
    {(["tools","vision","structured_output","fresh_information","uncertain"] as const).map(k=><Choice key={k} label={`${human(k)} condition`} value={c[k]==null ? "any" : c[k] ? "yes" : "no"} options={["any","yes","no"]} onChange={v=>patch({[k]:v==="any" ? null : v==="yes"})}/>)}
  </div><Multiple label="Tasks" values={c.tasks} options={tasks} onChange={v=>patch({tasks:v})}/>
    <Multiple label="Call purposes" values={c.purposes} options={purposes} onChange={v=>patch({purposes:v})}/>
    <details open={((c.all?.length ?? 0)+(c.any?.length ?? 0))>0}><summary>Condition groups</summary>
      <p className="ed-hint">All local fields must match. ALL requires every child; ANY requires one child when present. Up to four levels and sixteen children per group.</p>
      {(["all","any"] as const).map(group=><div key={group}><strong>{group.toUpperCase()}</strong>{(c[group] ?? []).map((child,i)=><fieldset key={i}><legend>{group.toUpperCase()} condition {i+1}</legend>
        <button type="button" className="btn btn-line sm" onClick={()=>patch({[group]:(c[group] ?? []).filter((_,j)=>j!==i)})}>Remove condition</button>
        <ConditionsEditor value={child} depth={depth+1} onChange={next=>patch({[group]:(c[group] ?? []).map((old,j)=>i===j ? next : old)})}/>
      </fieldset>)}<button type="button" className="btn btn-line sm" disabled={depth>=3 || (c.all?.length ?? 0)+(c.any?.length ?? 0)>=16}
        onClick={()=>patch({[group]:[...(c[group] ?? []),emptyConditions()]})}>Add {group} condition</button></div>)}
    </details>
  </div>;
}
