import { Box, Text } from '@devup-ui/react'
import type { MDXComponents } from 'mdx/types'
import { ComponentProps } from 'react'

export const _components = {
  h1(props: ComponentProps<typeof Text<'h1'>>) {
    return <Text as="h1" color="$title" typography="h1" {...props} />
  },
  h2(props: ComponentProps<typeof Text<'h2'>>) {
    return <Text as="h2" color="$title" typography="h2" {...props} />
  },
  h3(props: ComponentProps<typeof Text<'h3'>>) {
    return <Text as="h3" color="$title" typography="h3" {...props} />
  },
  h4(props: ComponentProps<typeof Text<'h4'>>) {
    return <Text as="h4" color="$title" typography="h4" {...props} />
  },
  h5(props: ComponentProps<typeof Text<'h5'>>) {
    return <Text as="h5" color="$title" typography="h5" {...props} />
  },
  p({ children }: { children: React.ReactNode }) {
    return (
      <Text as="p" color="$text" m="0">
        {children}
      </Text>
    )
  },
  table({ children }: { children: React.ReactNode }) {
    return (
      <Box
        as="table"
        border="none"
        selectors={{
          '& thead, & tbody': {
            border: 'none',
          },
        }}
      >
        {children}
      </Box>
    )
  },
  thead({ children }: { children: React.ReactNode }) {
    return (
      <Text
        as="thead"
        bg="$cardBg"
        border="none"
        color="$captionBold"
        m="0"
        textAlign="left"
      >
        {children}
      </Text>
    )
  },
  th({ children }: { children: React.ReactNode }) {
    return (
      <Text
        as="th"
        border="none"
        color="$captionBold"
        m="0"
        px="20px"
        py="14px"
      >
        {children}
      </Text>
    )
  },
  tr({ children }: { children: React.ReactNode }) {
    return (
      <Text
        as="tr"
        borderBottom="1px solid $border"
        borderTop="1px solid $border"
        color="$text"
        m="0"
        typography="body"
      >
        {children}
      </Text>
    )
  },
  td({ children }: { children: React.ReactNode }) {
    return (
      <Text
        as="td"
        border="none"
        color="$text"
        m="0"
        px="20px"
        py="14px"
        typography="body"
        whiteSpace="pre-wrap"
      >
        {children}
      </Text>
    )
  },
}

export function useMDXComponents(components: MDXComponents): MDXComponents {
  return {
    ...components,
    ..._components,
  }
}
