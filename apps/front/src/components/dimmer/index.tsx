'use client'
import { Box, keyframes } from '@devup-ui/react'
import clsx from 'clsx'
import { ComponentProps, useState } from 'react'

export const dim = keyframes({
  from: {
    opacity: 0,
  },
  to: {
    opacity: 1,
  },
})

export const brighten = keyframes({
  from: {
    opacity: 1,
  },
  to: {
    opacity: 0,
  },
})

export function Dimmer({
  dimmed = false,
  className,
  ...props
}: { dimmed?: boolean } & ComponentProps<typeof Box<'div'>>) {
  const [innerDimmed, setInnerDimmed] = useState(false)
  const render = dimmed || innerDimmed
  return (
    render && (
      <Box
        animationDuration="0.3s"
        animationFillMode="forwards"
        animationName={dimmed ? dim : brighten}
        bg="#000000B2"
        className={clsx(innerDimmed && 'dimmed', className)}
        inset="0"
        onAnimationEnd={() => setInnerDimmed(dimmed)}
        pos="fixed"
        styleOrder={1}
        zIndex="90"
        {...props}
      />
    )
  )
}
