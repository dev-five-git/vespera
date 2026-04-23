'use client'

import { Button, setTheme, useTheme } from '@devup-ui/react'
import { ComponentProps } from 'react'

export function ThemeToggle(props: ComponentProps<typeof Button<'button'>>) {
  const theme = useTheme() as 'light' | 'dark' | null
  return (
    <Button
      bg="transparent"
      border="none"
      cursor="pointer"
      onClick={(e) => {
        e.nativeEvent.stopImmediatePropagation()
        setTheme(theme === 'light' ? 'dark' : 'light')
      }}
      p="0"
      styleOrder={1}
      {...props}
    />
  )
}
