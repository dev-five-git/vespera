'use client'

import { useTheme } from '@devup-ui/react'

export function LightThemeBoundary({
  reverse = false,
  children,
}: {
  reverse?: boolean
  children?: React.ReactNode
}) {
  const theme = useTheme() as 'light' | 'dark' | null
  if (reverse) return theme === 'light' ? null : children
  return theme === 'light' ? children : null
}
