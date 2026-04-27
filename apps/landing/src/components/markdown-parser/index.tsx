'use client'
import { useEffect, useState } from 'react'
import rehypeSanitize from 'rehype-sanitize'
import rehypeStringify from 'rehype-stringify'
import remarkParse from 'remark-parse'
import remarkRehype from 'remark-rehype'
import { unified } from 'unified'

export function MarkdownParser({
  markdown,
  render,
}: {
  markdown: string
  render?: (content: string) => React.ReactNode
}) {
  const [contentHtml, setContentHtml] = useState('')
  useEffect(() => {
    ;(async () => {
      const file = await unified()
        .use(remarkParse) // Convert into markdown AST
        .use(remarkRehype) // Transform to HTML AST
        .use(rehypeSanitize) // Sanitize HTML input
        .use(rehypeStringify) // Convert AST into serialized HTML
        .process(markdown)
      const dom = new DOMParser().parseFromString(
        file.value as string,
        'text/html',
      )
      setContentHtml(dom.body.innerText)
    })()
  }, [markdown])
  return render ? render(contentHtml) : contentHtml
}
