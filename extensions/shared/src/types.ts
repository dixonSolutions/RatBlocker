/** Mirrors `ratblocker_core::types`. Kept in step by the compatibility tests. */

export type FilterDecision = 'allow' | 'block' | 'redirect' | 'remove_parameters';

export type ResourceType =
  | 'document'
  | 'script'
  | 'image'
  | 'stylesheet'
  | 'font'
  | 'media'
  | 'web_socket'
  | 'xml_http_request'
  | 'other'
  | 'subdocument'
  | 'object'
  | 'ping'
  | 'csp_report';

export interface RequestContext {
  request_url: string;
  source_url: string | null;
  application_id: string | null;
  resource_type: ResourceType;
}

export interface FilterResult {
  decision: FilterDecision;
  matched_rule_id: string | null;
  redirect_to?: string | null;
  rewritten_url?: string | null;
  removed_parameters?: string[];
}

export interface CosmeticResponse {
  hide: string[];
}

export interface EngineConfig {
  allowlisted_domains: string[];
  application_policies: Record<string, 'filter' | 'bypass'>;
  enabled: boolean;
}

/** Map a browser `webRequest` resource type onto the core's vocabulary. */
export function resourceTypeFromBrowser(type: string): ResourceType {
  switch (type) {
    case 'main_frame':
      return 'document';
    case 'sub_frame':
      return 'subdocument';
    case 'stylesheet':
      return 'stylesheet';
    case 'script':
      return 'script';
    case 'image':
    case 'imageset':
      return 'image';
    case 'font':
      return 'font';
    case 'object':
    case 'object_subrequest':
      return 'object';
    case 'xmlhttprequest':
      return 'xml_http_request';
    case 'ping':
    case 'beacon':
      return 'ping';
    case 'csp_report':
      return 'csp_report';
    case 'media':
      return 'media';
    case 'websocket':
      return 'web_socket';
    default:
      return 'other';
  }
}
