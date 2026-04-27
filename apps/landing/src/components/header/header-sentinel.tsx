'use client'

import { Box } from '@devup-ui/react'
import { ComponentPropsWithoutRef } from 'react'

import { useHeader } from './header-provider'

export function HeaderSentinel(
  props: ComponentPropsWithoutRef<typeof Box<'div'>>,
) {
  const { intersectionObserver, setIsSentinelVisible } = useHeader()
  return (
    <Box
      ref={(node) => {
        if (!node) return
        intersectionObserver?.observe(node)
        return () => {
          if (!intersectionObserver) return
          const ioEntries = intersectionObserver.takeRecords()
          ioEntries.some((entry) => entry.isIntersecting)
            ? setIsSentinelVisible(true)
            : setIsSentinelVisible(false)

          intersectionObserver?.unobserve(node)
        }
      }}
      {...props}
    />
  )
}
