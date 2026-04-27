'use client'
import { css } from '@devup-ui/react'

import { Hamburger } from '../mobile-menu/hamburger'
import { useHeader } from './header-provider'

export function HeaderHamburger() {
  const { isSentinelVisible } = useHeader()
  return (
    <Hamburger
      className={css({ color: isSentinelVisible ? '#FAFAFA' : '$title' })}
    />
  )
}
