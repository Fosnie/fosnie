import { useEffect,useState } from "react";
import { failureDescription,type RoutingFailure } from "@/api/routingFailure";
export function RoutingFailureNotice({failure,onChoose,onPrepareRetry}:{failure:RoutingFailure;onChoose?:()=>void;onPrepareRetry?:()=>void}) {
  const [until]=useState(()=>Math.min(failure.retry_at_ms??Infinity,Date.now()+(failure.retry_after_seconds??0)*1000));const [now,setNow]=useState(Date.now());
  useEffect(()=>{if(!failure.retry_after_seconds)return;const id=setInterval(()=>setNow(Date.now()),1000);return()=>clearInterval(id);},[failure.retry_after_seconds]);
  const remaining=Math.max(0,Math.ceil((until-now)/1000));
  return <div role={failure.code==="cancelled"?"status":"alert"} className="routing-failure" style={{padding:"12px",margin:"8px 0",border:"1px solid var(--border)",borderRadius:8}}>
    <p>{failureDescription(failure)}</p>
    {!failure.before_output && failure.code!=="cancelled" && <p className="ed-hint">Generation stopped. The received output is retained.</p>}
    {remaining>0 && <p className="ed-hint">Try again in {remaining} seconds.</p>}
    <div style={{display:"flex",gap:8}}>
      {onChoose && !["access_denied","infrastructure_unavailable"].includes(failure.code) && <button className="btn btn-line sm" type="button" onClick={onChoose}>Choose model</button>}
      {failure.can_retry && failure.before_output && onPrepareRetry && <button className="btn btn-line sm" type="button" disabled={remaining>0} onClick={onPrepareRetry}>Prepare retry</button>}
    </div>
  </div>;
}
