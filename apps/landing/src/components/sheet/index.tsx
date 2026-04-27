'use client'

import { Button } from '@devup-ui/react'
import {
  cloneElement,
  ComponentProps,
  createContext,
  Dispatch,
  isValidElement,
  SetStateAction,
  useCallback,
  useContext,
  useEffect,
  useState,
} from 'react'

import { SheetContainerAnimator } from './container-animator'

const SheetContext = createContext<{
  isOpen: boolean
  setIsOpen: Dispatch<SetStateAction<boolean>>
} | null>(null)

export function useSheet() {
  const context = useContext(SheetContext)
  if (!context) {
    throw new Error('useSheet must be used within a SheetProvider')
  }
  return context
}

export function SheetProvider({
  defaultOpen = false,
  open: openProp,
  onOpenChange,
  children,
}: {
  defaultOpen?: boolean
  open?: boolean
  onOpenChange?: (open: boolean | ((prev: boolean) => boolean)) => void
  children: React.ReactNode
}) {
  const [innerOpen, setInnerOpen] = useState(defaultOpen)
  const isOpen = openProp ?? innerOpen
  const handleOpenChange = useCallback(
    (open: boolean | ((prev: boolean) => boolean)) => {
      setInnerOpen(open)
      onOpenChange?.(open)
    },
    [onOpenChange],
  )

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        handleOpenChange(false)
      }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => {
      document.removeEventListener('keydown', handleKeyDown)
    }
  }, [handleOpenChange])

  return (
    <SheetContext.Provider value={{ isOpen, setIsOpen: handleOpenChange }}>
      {children}
    </SheetContext.Provider>
  )
}

export function SheetTrigger({
  asChild,
  children,
  ...props
}: ComponentProps<typeof Button<'button'>> & { asChild?: boolean }) {
  const { setIsOpen } = useSheet()

  if (asChild) {
    const child = isValidElement(children) ? children : null
    if (!child) return null
    return cloneElement(child, {
      onClick: () => setIsOpen((prev) => !prev),
      ...props,
    })
  }

  return (
    <Button
      bg="transparent"
      border="none"
      onClick={() => setIsOpen((prev) => !prev)}
      p="0"
      styleOrder={1}
      {...props}
    >
      {children}
    </Button>
  )
}

export function SheetContainer({
  ...props
}: Omit<
  ComponentProps<typeof SheetContainerAnimator>,
  'isOpen' | 'setIsOpen'
>) {
  const { isOpen, setIsOpen } = useSheet()
  return (
    <SheetContainerAnimator isOpen={isOpen} setIsOpen={setIsOpen} {...props} />
  )
}

export function SheetBoundary({
  children,
  reverse = false,
}: {
  children: React.ReactNode
  reverse?: boolean
}) {
  const { isOpen } = useSheet()
  if (reverse) return isOpen ? null : children
  return isOpen ? children : null
}

export function Sheet() {
  return <></>
}
