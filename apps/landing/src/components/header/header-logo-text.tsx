'use client'
import { Box } from '@devup-ui/react'

import { useHeader } from './header-provider'

export function HeaderLogoText() {
  const { isSentinelVisible } = useHeader()
  return (
    <Box
      bg={isSentinelVisible ? '#FAFAFA' : '$title'}
      h="28px"
      maskImage="url('/icons/logo-text.svg')"
      maskPos="center"
      maskRepeat="no-repeat"
      maskSize="contain"
      transition="all 0.2s ease-in-out"
      w="112px"
    />
  )
}
