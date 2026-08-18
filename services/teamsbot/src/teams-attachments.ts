import type { GraphTokenProvider } from './graph-token.js';
import type { FetchFn, JsonObject, TeamsApiAttachment } from './types.js';

const TEAMS_FILE_DOWNLOAD_INFO = 'application/vnd.microsoft.teams.file.download.info';

class AttachmentTooLargeError extends Error {
  constructor(maxBytes: number) {
    super(`Attachment exceeds ${maxBytes} bytes`);
  }
}

export type TeamsAttachmentDownloadOptions = {
  allowedHosts: readonly string[];
  enabled: boolean;
  fetchFn?: FetchFn;
  graphTokenProvider?: GraphTokenProvider;
  graphTokenScope?: string;
  maxBytes: number;
};

export async function hydrateTeamsAttachments(
  attachments: TeamsApiAttachment[],
  options: TeamsAttachmentDownloadOptions,
): Promise<TeamsApiAttachment[]> {
  if (!options.enabled || attachments.length === 0) {
    return attachments.map(redactTeamsDownloadAttachment);
  }
  const hydrated: TeamsApiAttachment[] = [];
  for (const attachment of attachments) {
    hydrated.push(await hydrateTeamsAttachment(attachment, options));
  }
  return hydrated;
}

async function hydrateTeamsAttachment(
  attachment: TeamsApiAttachment,
  options: TeamsAttachmentDownloadOptions,
): Promise<TeamsApiAttachment> {
  const candidate = resolveDownloadCandidate(attachment);
  if (!candidate) {
    return redactTeamsDownloadAttachment(attachment);
  }
  if (!isAllowedHost(candidate.url, options.allowedHosts)) {
    return {
      ...redactTeamsDownloadAttachment(attachment),
      fetchError: `Attachment host is not allowed: ${safeHost(candidate.url)}`,
    };
  }

  try {
    const response = await fetchWithOptionalGraphAuth(candidate.url, options);
    if (!response.ok) {
      await response.body?.cancel();
      return {
        ...redactTeamsDownloadAttachment(attachment),
        contentUrl: undefined,
        fetchError: `Attachment download failed: ${response.status} ${response.statusText}`,
      };
    }
    const contentLength = parseContentLength(response.headers.get('content-length'));
    if (contentLength !== undefined && contentLength > options.maxBytes) {
      await response.body?.cancel();
      return {
        ...redactTeamsDownloadAttachment(attachment),
        contentUrl: undefined,
        fetchError: `Attachment exceeds ${options.maxBytes} bytes`,
      };
    }
    const data = await readResponseBodyWithLimit(response, options.maxBytes);
    return {
      ...redactTeamsDownloadAttachment(attachment),
      contentType: response.headers.get('content-type') ?? candidate.contentType ?? attachment.contentType,
      dataBase64: data.toString('base64'),
      name: attachment.name ?? candidate.fileName,
    };
  } catch (error) {
    return {
      ...redactTeamsDownloadAttachment(attachment),
      contentUrl: undefined,
      fetchError: attachmentDownloadErrorMessage(error),
    };
  }
}

async function fetchWithOptionalGraphAuth(
  url: string,
  options: TeamsAttachmentDownloadOptions,
): Promise<Response> {
  const fetchFn = options.fetchFn ?? fetch;
  const first = await fetchFn(url);
  if (first.ok || (first.status !== 401 && first.status !== 403)) {
    return first;
  }
  if (!looksLikeGraphBackedUrl(url)) {
    return first;
  }
  const token = await options.graphTokenProvider?.getAccessToken(options.graphTokenScope ?? 'https://graph.microsoft.com/.default');
  if (!token) {
    return first;
  }
  await first.body?.cancel();
  return fetchFn(url, { headers: { authorization: `Bearer ${token}` } });
}

function resolveDownloadCandidate(attachment: TeamsApiAttachment): {
  contentType?: string;
  fileName?: string;
  url: string;
} | undefined {
  if (attachment.contentType.toLowerCase() === TEAMS_FILE_DOWNLOAD_INFO) {
    const content = isRecord(attachment.content) ? attachment.content : undefined;
    const downloadUrl = stringField(content, 'downloadUrl');
    if (!downloadUrl) {
      return undefined;
    }
    const fileName = attachment.name ?? stringField(content, 'fileName');
    const fileType = stringField(content, 'fileType');
    return {
      contentType: fileType ? mimeFromExtension(fileType) : undefined,
      fileName,
      url: downloadUrl,
    };
  }
  return attachment.contentUrl
    ? { contentType: attachment.contentType, fileName: attachment.name, url: attachment.contentUrl }
    : undefined;
}

async function readResponseBodyWithLimit(response: Response, maxBytes: number): Promise<Buffer> {
  if (!response.body) {
    return Buffer.alloc(0);
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let totalBytes = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        return Buffer.concat(chunks, totalBytes);
      }
      totalBytes += value.byteLength;
      if (totalBytes > maxBytes) {
        throw new AttachmentTooLargeError(maxBytes);
      }
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function redactTeamsDownloadAttachment(attachment: TeamsApiAttachment): TeamsApiAttachment {
  const redacted = attachment.dataBase64 ? attachment : { ...attachment, contentUrl: undefined };
  if (attachment.contentType.toLowerCase() !== TEAMS_FILE_DOWNLOAD_INFO || !isRecord(attachment.content)) {
    return redacted;
  }
  const content = { ...attachment.content };
  delete content.downloadUrl;
  content.downloadUrlRedacted = true;
  return { ...redacted, content };
}

function isAllowedHost(rawUrl: string, allowedHosts: readonly string[]): boolean {
  let host: string;
  try {
    const url = new URL(rawUrl);
    if (url.protocol !== 'https:') {
      return false;
    }
    host = url.hostname.toLowerCase();
  } catch {
    return false;
  }
  return allowedHosts.some((allowed) => {
    const normalized = allowed.trim().toLowerCase();
    if (!normalized) {
      return false;
    }
    if (normalized.startsWith('*.')) {
      const suffix = normalized.slice(1);
      return host.endsWith(suffix) && host.length > suffix.length;
    }
    return host === normalized;
  });
}

function looksLikeGraphBackedUrl(rawUrl: string): boolean {
  const host = safeHost(rawUrl);
  return host === 'graph.microsoft.com' || host.endsWith('.sharepoint.com') || host.endsWith('.1drv.ms');
}

function safeHost(rawUrl: string): string {
  try {
    return new URL(rawUrl).hostname.toLowerCase();
  } catch {
    return 'invalid-url';
  }
}

function attachmentDownloadErrorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return redactUrls(message);
}

function redactUrls(value: string): string {
  return value.replace(/https?:\/\/[^\s'"<>]+/gi, (rawUrl) => {
    const host = safeHost(rawUrl);
    return host === 'invalid-url' ? '[redacted-url]' : `[redacted-url:${host}]`;
  });
}

function parseContentLength(value: string | null): number | undefined {
  if (!value) {
    return undefined;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined;
}

function stringField(value: JsonObject | undefined, key: string): string | undefined {
  const field = value?.[key];
  return typeof field === 'string' && field.trim() ? field : undefined;
}

function isRecord(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function mimeFromExtension(extension: string): string | undefined {
  switch (extension.toLowerCase().replace(/^\./, '')) {
    case 'csv':
      return 'text/csv';
    case 'xlsx':
      return 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
    case 'xls':
      return 'application/vnd.ms-excel';
    case 'pdf':
      return 'application/pdf';
    case 'txt':
      return 'text/plain';
    default:
      return undefined;
  }
}
