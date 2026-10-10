export type RoutingFailure = {schema_version:1;code:string;source:"provider"|"routing";before_output:boolean;can_retry:boolean;retry_after_seconds?:number|null;retry_at_ms?:number;message?:string};
const descriptions:Record<string,string>={
  provider_connection:"Could not connect to the model provider.",provider_timeout:"The model did not respond within the allowed time.",
  provider_rate_limited:"The provider is rate limited. Wait or choose another permitted model.",
  provider_quota_exhausted:"The provider reported an exhausted quota or spending allowance. Choose another permitted model or contact its manager.",
  provider_payment_required:"The provider requires a billing update. Choose another permitted model or contact its manager.",
  provider_credentials:"The provider rejected its credentials or access. Check your own key or contact its manager.",
  provider_request_rejected:"The provider rejected the request. Check model configuration and context limits.",
  provider_unavailable:"The model is temporarily unavailable.",access_denied:"Access is unavailable. Check permissions before retrying.",
  configuration_changed:"Routing configuration changed. Reload it before trying again.",run_budget:"The routing allowance is exhausted. Review the run limits.",
  request_incompatible:"No execution path supports this request. Review model capabilities and context limits.",
  no_eligible_model:"No model meets the current permissions, capabilities and limits.",cancelled:"Generation stopped.",
  infrastructure_unavailable:"The inference service is unavailable. Try again later.",
  routing_unavailable:"Routing could not finish. Check the configuration or try again later.",
};
export function routingFailure(value:unknown):RoutingFailure|undefined {
  if(!value || typeof value!=="object")return;
  const v=value as Record<string,unknown>;
  if(v.schema_version!==1 || typeof v.code!=="string" || !Object.prototype.hasOwnProperty.call(descriptions,v.code)
    || !["provider","routing"].includes(String(v.source)) || typeof v.before_output!=="boolean" || typeof v.can_retry!=="boolean")return;
  const after=v.retry_after_seconds;
  if(after!=null && (!Number.isInteger(after)||Number(after)<1||Number(after)>86400))return;
  return {schema_version:1,code:v.code,source:v.source as "provider"|"routing",before_output:v.before_output,
    can_retry:v.can_retry,retry_after_seconds:after as number|null|undefined,
    retry_at_ms:typeof v.retry_at_ms==="number"&&Number.isSafeInteger(v.retry_at_ms)&&v.retry_at_ms>0?v.retry_at_ms:undefined};
}
export function failureDescription(value:RoutingFailure):string {return descriptions[value.code]??descriptions.routing_unavailable;}
