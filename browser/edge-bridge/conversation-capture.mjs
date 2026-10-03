'use strict';

export function conversationRoute(remoteId) {
  if (typeof remoteId !== 'string' || remoteId.length === 0 || remoteId.length > 256) {
    return null;
  }
  return `https://chatgpt.com/c/${encodeURIComponent(remoteId)}`;
}

export function matchConversationResponse(response, remoteId) {
  if (!response || typeof response !== 'object') return null;
  if (typeof remoteId !== 'string' || remoteId.length === 0 || remoteId.length > 256) {
    return null;
  }

  let url;
  try {
    url = new URL(response.url);
  } catch {
    return null;
  }
  if (url.origin !== 'https://chatgpt.com') return null;

  const prefix = '/backend-api/conversations/';
  if (!url.pathname.startsWith(prefix)) return null;

  const encodedId = url.pathname.slice(prefix.length);
  if (!encodedId || encodedId.includes('/')) return null;

  let decodedId;
  try {
    decodedId = decodeURIComponent(encodedId);
  } catch {
    return null;
  }
  if (decodedId !== remoteId) return null;

  const status = Number(response.status);
  if (!Number.isInteger(status) || status < 100 || status > 599) return null;

  return {
    path: url.pathname,
    query_keys: [...url.searchParams.keys()],
    http_status: status,
    mime_type: typeof response.mimeType === 'string' ? response.mimeType : '',
  };
}

export function isJsonMimeType(mimeType) {
  if (typeof mimeType !== 'string') return false;
  const mediaType = mimeType.toLowerCase().split(';', 1)[0].trim();
  return mediaType === 'application/json' || mediaType.endsWith('+json');
}

export function classifyConversationHttpStatus(status) {
  if (status === 200) return 'success';
  if (status === 429) return 'transient_rate_limit';
  return 'terminal_http_error';
}

export function selectFinalConversationResponseMeta(responseMeta, lastRateLimitMeta) {
  return responseMeta ?? lastRateLimitMeta ?? null;
}
