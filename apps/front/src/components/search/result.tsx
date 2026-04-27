'use client'

import { Center, css, Grid, Text, VStack } from '@devup-ui/react'
import Link from 'next/link'
import { ComponentProps, useEffect, useMemo, useState } from 'react'

import { MarkdownParser } from '../markdown-parser'
import { useSearchContext } from './provider'

export function Result(
  props: Omit<ComponentProps<typeof VStack<'div'>>, 'children'>,
) {
  const { debouncedValue: query, insideClickRefs } = useSearchContext()
  const inputRef = Array.from(insideClickRefs.current).find(
    (el) => el.tagName === 'INPUT',
  )
  const inputRect = inputRef?.getBoundingClientRect()
  const desktopLeft = inputRect
    ? inputRect.left - inputRect.width + 16 + 'px'
    : '50%'

  const [data, setData] = useState<
    {
      title: string
      text: string
      url: string
    }[]
  >()
  useEffect(() => {
    if (query) {
      fetch('/search.json')
        .then((response) => response.json())
        .then(
          (
            data: {
              title: string
              text: string
              url: string
            }[],
          ) => {
            setData(
              data
                .filter(Boolean)
                .filter(
                  (item) =>
                    item.title.toLowerCase().includes(query.toLowerCase()) ||
                    item.text.toLowerCase().includes(query.toLowerCase()),
                ),
            )
          },
        )
    }
  }, [query])
  const reg = useMemo(() => new RegExp(`(${query})`, 'gi'), [query])
  return (
    <Grid
      bg="$containerBackground"
      borderRadius="16px"
      h="max-content"
      left={['50%', null, null, desktopLeft]}
      pl="$spacingSpacing08"
      pos="fixed"
      pr="$spacingSpacing16"
      py="$spacingSpacing20"
      styleOrder={1}
      top={['88px', null, null, '75px']}
      transform={['translateX(-50%)', null, null, 'unset']}
      w={['calc(100% - 32px)', null, null, '500px']}
      zIndex="120"
      {...props}
    >
      <VStack
        bg="$containerBackground"
        h="max-content"
        maxH="800px"
        overflowY="auto"
        selectors={{
          '&::-webkit-scrollbar-thumb': {
            bg: '$border',
            borderRadius: '8px',
            border: '16px solid transparent',
          },
          '&::-webkit-scrollbar': {
            w: '6px',
          },
        }}
        w={[null, null, null, '468px']}
      >
        {data?.length ? (
          <VStack gap="16px">
            {data.map((item, i) => (
              <Link
                key={item.url + i}
                className={css({ display: 'contents' })}
                href={item.url}
              >
                <VStack>
                  <Text color="$title" px="16px" typography="tinyB">
                    {item.url}
                  </Text>
                  <VStack px="16px" py="12px">
                    <Text color="$title" typography="menu">
                      {item.title}
                    </Text>
                    <Text
                      WebkitBoxOrient="vertical"
                      WebkitLineClamp="6"
                      color="$title"
                      display="-webkit-box"
                      overflow="hidden"
                      textOverflow="ellipsis"
                      typography="caption"
                      whiteSpace="pre-wrap"
                    >
                      <MarkdownParser
                        markdown={item.text}
                        render={(content) => (
                          <Text>
                            {content.split(reg).map((part, idx) =>
                              part.toLowerCase() === query.toLowerCase() ? (
                                <Text
                                  key={idx}
                                  color="$vespertidePrimary"
                                  fontWeight="bold"
                                >
                                  {part}
                                </Text>
                              ) : (
                                part
                              ),
                            )}
                          </Text>
                        )}
                      />
                    </Text>
                  </VStack>
                </VStack>
              </Link>
            ))}
          </VStack>
        ) : (
          <Center py="40px">
            <Text color="$caption" textAlign="center" typography="caption">
              No search results found.
            </Text>
          </Center>
        )}
      </VStack>
    </Grid>
  )
}

export { Result as SearchResult }
