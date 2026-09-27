import { expect, it } from 'bun:test';
import { Message, parseMarkdown } from 'chat';
import { serializeMessage } from '../src/session-api';

it.each(['buffer', 'array-buffer'] as const)('serializes %s attachments supplied by the SDK', async (format) => {
  const bytes = new TextEncoder().encode('attachment content');
  const data = format === 'buffer' ? Buffer.from(bytes) : bytes.buffer;
  const message = new Message({
    id: 'message', threadId: 'thread', text: 'See attached', formatted: parseMarkdown('See attached'), raw: {},
    author: { userId: 'user', userName: 'user', fullName: 'User', isBot: false, isMe: false },
    metadata: { dateSent: new Date(), edited: false },
    attachments: [
      { type: 'file', name: 'inline.txt', data: format === 'buffer' ? Buffer.from(bytes) : new Blob([bytes]) },
      { type: 'file', name: 'download.txt', fetchData: async () => data },
    ],
  });
  const result = await serializeMessage(message);
  expect(result.attachments.map(a => a.dataBase64)).toEqual([
    Buffer.from(bytes).toString('base64'), Buffer.from(bytes).toString('base64'),
  ]);
});
