'use client'

import { Center } from '@devup-ui/react'
import { ComponentProps } from 'react'

import { useHeader } from './header-provider'

export function HeaderContainer(props: ComponentProps<typeof Center<'div'>>) {
  const { transparent } = useHeader()
  return (
    <Center
      backdropFilter={transparent ? 'blur(20px)' : 'none'}
      bg={transparent ? '#FFFFFF03' : '$containerBackground'}
      flexDir="column"
      left="0px"
      pl={['16px', null, null, null, 'initial']}
      pos="fixed"
      pr={['4px', null, null, null, 'initial']}
      py={['12px', null, null, null, '4px']}
      right="0px"
      styleOrder={1}
      top="0px"
      transition="all .1s"
      zIndex="100"
      {...props}
    />
  )
}
