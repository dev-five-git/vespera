'use client'

import { Flex, Text } from '@devup-ui/react'
import Link from 'next/link'
import { createContext, useContext } from 'react'

import { Content, useTableOfContents } from '.'

const ContentContext = createContext<{
  content: Content
} | null>(null)

export function useContent() {
  const context = useContext(ContentContext)
  if (!context) {
    throw new Error('useContent must be used within a ContentProvider')
  }
  return context
}

export function Iterator({ children }: { children?: React.ReactNode }) {
  const { contents } = useTableOfContents()
  return contents.map((content) => (
    <ContentContext.Provider key={content.value} value={{ content }}>
      {children}
    </ContentContext.Provider>
  ))
}

function Anchor() {
  const { highlightedContent } = useTableOfContents()
  const { content } = useContent()
  const isSelected = highlightedContent?.value === content.value
  return (
    <Link href={`#${content.value}`}>
      <Flex
        alignItems="center"
        borderRadius="$spacingSpacing08"
        py="$spacingSpacing08"
        role="group"
      >
        <Text
          _groupHover={{
            color: '$title',
          }}
          color={isSelected ? '$vesperaPrimary' : '$caption'}
          flex="1"
          typography="buttonSm"
        >
          {content.label}
        </Text>
      </Flex>
    </Link>
  )
}

export { Anchor as TableOfContentsAnchor, Iterator as TableOfContentsIterator }
