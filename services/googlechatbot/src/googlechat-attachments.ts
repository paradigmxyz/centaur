import type { Attachment } from 'chat';
import type { GoogleChatApiAttachment } from './types.js';

export type GoogleChatAttachmentDownloadOptions = {
  enabled: boolean;
  maxBytes: number;
};

/**
 * Download Google Chat attachments into base64 parts, when enabled.
 *
 * The adapter's `fetchData()` calls `media.download` with the Chat app's own
 * credentials, so nothing here handles URLs or tokens: an attachment either has
 * a `attachmentDataRef` the app may read, or it is reported as unavailable. The
 * size cap is enforced after the download because the Chat API returns the whole
 * body; oversize payloads are dropped rather than forwarded.
 */
export async function hydrateGoogleChatAttachments(
  attachments: GoogleChatApiAttachment[],
  sdkAttachments: readonly Attachment[] | undefined,
  options: GoogleChatAttachmentDownloadOptions,
): Promise<GoogleChatApiAttachment[]> {
  if (!options.enabled || attachments.length === 0) {
    return attachments;
  }
  const hydrated: GoogleChatApiAttachment[] = [];
  for (const [index, attachment] of attachments.entries()) {
    hydrated.push(await hydrateAttachment(attachment, sdkAttachments?.[index], options));
  }
  return hydrated;
}

async function hydrateAttachment(
  attachment: GoogleChatApiAttachment,
  sdkAttachment: Attachment | undefined,
  options: GoogleChatAttachmentDownloadOptions,
): Promise<GoogleChatApiAttachment> {
  const fetchData = sdkAttachment?.fetchData;
  const data = sdkAttachment?.data;
  if (!fetchData && !data) {
    return {
      ...attachment,
      fetchError: 'Attachment has no Google Chat data reference to download',
    };
  }
  if (typeof sdkAttachment?.size === 'number' && sdkAttachment.size > options.maxBytes) {
    return {
      ...attachment,
      fetchError: `Attachment exceeds ${options.maxBytes} bytes`,
    };
  }
  try {
    const buffer = await toBuffer(data ?? await fetchData!());
    if (buffer.byteLength > options.maxBytes) {
      return {
        ...attachment,
        fetchError: `Attachment exceeds ${options.maxBytes} bytes`,
      };
    }
    return {
      ...attachment,
      contentType: sdkAttachment?.mimeType ?? attachment.contentType,
      dataBase64: buffer.toString('base64'),
      name: attachment.name ?? sdkAttachment?.name,
    };
  } catch (error) {
    return {
      ...attachment,
      fetchError: attachmentDownloadErrorMessage(error),
    };
  }
}

async function toBuffer(data: Buffer | Blob | ArrayBuffer): Promise<Buffer> {
  if (Buffer.isBuffer(data)) {
    return data;
  }
  if (data instanceof ArrayBuffer) {
    return Buffer.from(data);
  }
  return Buffer.from(await data.arrayBuffer());
}

function attachmentDownloadErrorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return redactUrls(message);
}

function redactUrls(value: string): string {
  return value.replace(/https?:\/\/[^\s'"<>]+/gi, (rawUrl) => {
    try {
      return `[redacted-url:${new URL(rawUrl).hostname.toLowerCase()}]`;
    } catch {
      return '[redacted-url]';
    }
  });
}
