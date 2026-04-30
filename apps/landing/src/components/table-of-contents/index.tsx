'use client'

import { usePathname } from 'next/navigation'
import { createContext, useContext, useEffect, useMemo, useState } from 'react'

export interface Content {
  label: string
  value: string
  pathname: string
}

function sortContentsByDomOrder(items: Content[]): Content[] {
  if (items.length <= 1) return items
  return [...items].sort((a, b) => {
    const elA = document.getElementById(a.value)
    const elB = document.getElementById(b.value)
    if (!elA || !elB) return 0
    const bit = elA.compareDocumentPosition(elB)
    if (bit & Node.DOCUMENT_POSITION_FOLLOWING) return -1
    if (bit & Node.DOCUMENT_POSITION_PRECEDING) return 1
    return 0
  })
}

const TableOfContentsContext = createContext<{
  contents: Content[]
  setContents: (contents: Content[]) => void
  highlightedContent: Content | null
  intersectionObserver: IntersectionObserver | null
} | null>(null)

export function useTableOfContents() {
  const context = useContext(TableOfContentsContext)
  if (!context) {
    throw new Error(
      'useTableOfContents must be used within a TableOfContentsProvider',
    )
  }
  return context
}

export function TableOfContentsProvider({
  children,
}: {
  children: React.ReactNode
}) {
  const pathname = usePathname()
  const [contents, setContents] = useState<Content[]>([])
  const [intersectingContents, setIntersectingContents] = useState<Content[]>(
    [],
  )
  if (intersectingContents.some((content) => content.pathname !== pathname)) {
    setIntersectingContents((prev) =>
      prev.filter((content) => content.pathname === pathname),
    )
  }

  const highlightedContent = intersectingContents[0] ?? null

  const io = useMemo(() => {
    if (typeof window === 'undefined') return null
    return new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!(entry.target instanceof HTMLElement)) return
        const item = {
          label: entry.target.innerText ?? '',
          value: entry.target.id ?? '',
          pathname: pathname ?? '',
        }
        setContents((prev) =>
          prev.some((content) => content.value === item.value)
            ? prev
            : [...prev, item],
        )
        if (entry.isIntersecting) {
          setIntersectingContents((prev) =>
            sortContentsByDomOrder([...prev, item]),
          )
        } else {
          setIntersectingContents((prev) =>
            sortContentsByDomOrder(
              prev.filter((content) => content.value !== entry.target.id),
            ),
          )
        }
      })
    })
  }, [pathname])

  useEffect(() => {
    if (!io) return
    const elements = document.querySelectorAll(
      '.markdown-body h2, .markdown-body h3, .markdown-body h4, .markdown-body h5, .markdown-body h6',
    )
    elements.forEach((element) => {
      io.observe(element)
    })
    return () => {
      elements.forEach((element) => {
        io.unobserve(element)
        setContents((prev) =>
          prev.filter((content) => content.value !== element.id),
        )
      })
    }
  }, [io, pathname])

  return (
    <TableOfContentsContext.Provider
      value={{
        contents,
        setContents,
        highlightedContent,
        intersectionObserver: io,
      }}
    >
      {children}
    </TableOfContentsContext.Provider>
  )
}

export function TableOfContents() {}
