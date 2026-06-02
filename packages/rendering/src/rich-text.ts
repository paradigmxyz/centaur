import type { RendererTaskBlock } from './types'

type InlineText = {
  type: 'text'
  text: string
  style?: { bold?: boolean; italic?: boolean; strike?: boolean; code?: boolean }
}

export function section(elements: InlineText[]): RendererTaskBlock {
  return { type: 'text', text: elements.map(element => element.text).join('') }
}

export function preformatted(text: string, language?: string): RendererTaskBlock {
  return {
    type: 'code',
    text,
    ...(language ? { language } : {}),
  }
}

export function text(text: string, style?: InlineText['style']): InlineText {
  return { type: 'text', text, ...(style ? { style } : {}) }
}

export function elementsToPlainText(elements: RendererTaskBlock[]): string {
  return elements.map(elementToPlainText).filter(Boolean).join('\n')
}

function elementToPlainText(element: RendererTaskBlock): string {
  if (element.type === 'code') {
    return element.text
  }
  return element.text
}
