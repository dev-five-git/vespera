'use client'
import { Box, css } from '@devup-ui/react'
import clsx from 'clsx'
import { ComponentProps, useEffect, useRef, useState } from 'react'

import * as keyframes from './keyframes'

const positions = {
  left: css({
    styleOrder: 1,
    top: 0,
    bottom: 0,
    left: 0,
    borderTopRightRadius: '20px',
    borderBottomRightRadius: '20px',
    transform: 'translateX(-100%)',
    boxShadow: '8px 12px 24px 0 #0000001A',
  }),
  right: css({
    styleOrder: 1,
    top: 0,
    bottom: 0,
    right: 0,
    borderTopLeftRadius: '20px',
    borderBottomLeftRadius: '20px',
    transform: 'translateX(100%)',
    boxShadow: '-8px 12px 24px 0 #0000001A',
  }),
  top: css({
    styleOrder: 1,
    left: 0,
    right: 0,
    top: 0,
    borderBottomLeftRadius: '20px',
    borderBottomRightRadius: '20px',
    transform: 'translateY(-100%)',
    boxShadow: '0px 8px 24px 0 #0000001A',
  }),
  bottom: css({
    styleOrder: 1,
    left: 0,
    right: 0,
    bottom: 0,
    borderTopLeftRadius: '20px',
    borderTopRightRadius: '20px',
    transform: 'translateY(100%)',
    boxShadow: '0px -8px 24px 0 #0000001A',
  }),
}
export function ContainerAnimator({
  position = 'right',
  className,
  isOpen,
  setIsOpen,
  ref: refProp,
  ...props
}: ComponentProps<typeof Box<'div'>> & {
  position?: 'left' | 'right' | 'top' | 'bottom'
  isOpen: boolean
  setIsOpen: (isOpen: boolean) => void
}) {
  const ref = useRef<HTMLDivElement>(null)
  const [innerOpen, setInnerOpen] = useState(false)
  const render = isOpen || innerOpen

  useEffect(() => {
    function handleOutsideClick(event: MouseEvent) {
      if (!isOpen) return
      if (ref.current && !ref.current.contains(event.target as Node)) {
        setIsOpen(false)
      }
    }
    document.addEventListener('click', handleOutsideClick)
    return () => document.removeEventListener('click', handleOutsideClick)
  }, [setIsOpen, isOpen])

  useEffect(() => {
    if (isOpen) {
      document.body.style.setProperty('overflow', 'hidden')
    } else {
      document.body.style.removeProperty('overflow')
    }
  }, [isOpen])

  return (
    render && (
      <Box
        ref={(node) => {
          if (refProp) {
            if (typeof refProp === 'function') {
              refProp(node)
            } else {
              refProp.current = node
            }
          }
          if (!node?.checkVisibility()) return () => {}
          ref.current = node
          return () => {
            ref.current = null
          }
        }}
        animationDuration="0.3s"
        animationFillMode="forwards"
        animationTimingFunction="ease-in-out"
        aria-label="side-bar"
        bg="$containerBackground"
        className={clsx(
          positions[position as keyof typeof positions],
          className,
        )}
        onAnimationEnd={() => setInnerOpen(isOpen)}
        pos="fixed"
        style={{
          animationName: {
            left: { open: keyframes.leftOpen, close: keyframes.leftClose },
            right: { open: keyframes.rightOpen, close: keyframes.rightClose },
            top: { open: keyframes.topOpen, close: keyframes.topClose },
            bottom: {
              open: keyframes.bottomOpen,
              close: keyframes.bottomClose,
            },
          }[position as keyof typeof positions][isOpen ? 'open' : 'close'],
        }}
        styleOrder={1}
        w={['100%', null, null, null, 'unset']}
        zIndex={100}
        {...props}
      />
    )
  )
}

export { ContainerAnimator as SheetContainerAnimator }
