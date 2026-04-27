'use client'
import { ComponentProps } from 'react'

import { useSearchContext } from '@/components/search/provider'
import { SheetRouteContainer } from '@/components/sheet/router'

export function RouteContainer(
  props: ComponentProps<typeof SheetRouteContainer>,
) {
  const { insideClickRefs } = useSearchContext()
  return (
    <SheetRouteContainer
      ref={(el) => {
        if (!el) return
        insideClickRefs.current.add(el)
        return () => {
          insideClickRefs.current.delete(el)
        }
      }}
      {...props}
    />
  )
}

export { RouteContainer as SearchSheetRouteContainer }
