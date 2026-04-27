'use client'
import { css } from '@devup-ui/react'
import { ComponentProps } from 'react'

import { GnbIcon } from './gnb-icon'
import { useHeader } from './header-provider'

export function HeaderGnbIcon(props: ComponentProps<typeof GnbIcon>) {
  const { isSentinelVisible } = useHeader()

  return (
    <GnbIcon
      className={css({ bg: isSentinelVisible ? '#FAFAFA' : '$title' })}
      {...props}
    />
  )
}
