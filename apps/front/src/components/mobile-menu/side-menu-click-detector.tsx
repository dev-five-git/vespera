'use client'
import { VStack } from '@devup-ui/react'
import { ComponentProps, MouseEvent } from 'react'

import { useSheet } from '../sheet'

export function SideMenuClickDetector(
  props: ComponentProps<typeof VStack<'div'>>,
) {
  const { setIsOpen } = useSheet()
  const handleClick = (e: MouseEvent) => {
    if (!(e.target instanceof HTMLElement)) return
    const sideMenuElement = e.target.closest('div[aria-label="side menu"]')
    if (!sideMenuElement || sideMenuElement?.getAttribute('aria-expanded'))
      return
    setIsOpen(false)
  }
  return <VStack onClick={handleClick} {...props} />
}
