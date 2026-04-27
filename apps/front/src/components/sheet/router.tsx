'use client'
import { Button } from '@devup-ui/react'
import {
  cloneElement,
  ComponentProps,
  createContext,
  isValidElement,
  useContext,
  useState,
} from 'react'

import { SheetProvider } from '.'
import { SheetContainerAnimator } from './container-animator'

const SheetRouterContext = createContext<{
  route: string | null
  setRoute: (route: string | null) => void
} | null>(null)

export function useSheetRouter() {
  const context = useContext(SheetRouterContext)
  if (!context) {
    throw new Error('useSheetRouter must be used within a SheetRouter')
  }
  return context
}

export function Router({
  defaultRoute = null,
  children,
}: {
  defaultRoute?: string | null
  children?: React.ReactNode
}) {
  const [route, setRoute] = useState(defaultRoute)
  return (
    <SheetRouterContext.Provider value={{ route, setRoute }}>
      {children}
    </SheetRouterContext.Provider>
  )
}

function Route({
  name,
  ...props
}: Omit<
  ComponentProps<typeof SheetProvider>,
  'open' | 'onOpenChange' | 'defaultOpen'
> & { name: string }) {
  const { route, setRoute } = useSheetRouter()
  const open = route === name
  const handleOpenChange = (
    nextOpen: boolean | ((prev: boolean) => boolean),
  ) => {
    if (typeof nextOpen === 'function') {
      setRoute(nextOpen(open) ? name : null)
    } else {
      setRoute(nextOpen ? name : null)
    }
  }
  return (
    <SheetProvider onOpenChange={handleOpenChange} open={open} {...props} />
  )
}

function RouteTrigger({
  name,
  children,
  asChild,
  ...props
}: ComponentProps<typeof Button<'button'>> & {
  asChild?: boolean
  name: string
}) {
  const { route, setRoute } = useSheetRouter()

  if (asChild) {
    const child = isValidElement(children) ? children : null
    if (!child) return null
    return cloneElement(child, {
      onClick: () => setRoute(route ? null : name),
      ...props,
    })
  }

  return (
    <Button
      bg="transparent"
      border="none"
      onClick={() => setRoute(route ? null : name)}
      p="0"
      styleOrder={1}
      {...props}
    >
      {children}
    </Button>
  )
}

function Container({
  name,
  ...props
}: Omit<
  ComponentProps<typeof SheetContainerAnimator>,
  'isOpen' | 'setIsOpen'
> & { name: string }) {
  const { route, setRoute } = useSheetRouter()
  const open = route === name
  return (
    <SheetContainerAnimator
      isOpen={open}
      setIsOpen={(nextOpen) => setRoute(nextOpen ? name : null)}
      {...props}
    />
  )
}

function Boudnary({
  name,
  children,
  reverse = false,
}: {
  name: string
  children?: React.ReactNode
  reverse?: boolean
}) {
  const { route } = useSheetRouter()
  if (reverse) return route === name ? null : children
  return route === name ? children : null
}

export {
  Route as SheetRoute,
  Boudnary as SheetRouteBoundary,
  Container as SheetRouteContainer,
  Router as SheetRouter,
  RouteTrigger as SheetRouteTrigger,
}
