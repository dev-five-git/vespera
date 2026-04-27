'use client'

import { Center } from '@devup-ui/react'
import { ComponentProps } from 'react'

import { useSearchContext } from '../search/provider'
import { useHeader } from './header-provider'

export function HeaderContainer(props: ComponentProps<typeof Center<'div'>>) {
  const { transparent } = useHeader()
  const { dimmed } = useSearchContext()
  return (
    <Center
      backdropFilter={transparent ? 'blur(20px)' : 'none'}
      bg={transparent && !dimmed ? '#FFFFFF03' : '$containerBackground'}
      flexDir="column"
      left="0px"
      pl="16px"
      pos="fixed"
      pr="4px"
      py="12px"
      right="0px"
      styleOrder={1}
      top="0px"
      transition="all .1s"
      zIndex="100"
      {...props}
    />
  )
}
