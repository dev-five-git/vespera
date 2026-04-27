'use client'
import { ComponentProps } from 'react'

import { Dimmer as BaseDimmer } from '../dimmer'
import { useSheetRouter } from '../sheet/router'
import { useSearchContext } from './provider'

export function Dimmer(props: ComponentProps<typeof BaseDimmer>) {
  const { dimmed } = useSearchContext()
  const { route } = useSheetRouter()
  return <BaseDimmer dimmed={dimmed || route === 'search'} {...props} />
}

export { Dimmer as SearchDimmer }
