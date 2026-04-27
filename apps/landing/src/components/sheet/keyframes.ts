import { keyframes } from '@devup-ui/react'

export const topOpen = keyframes({
  from: {
    transform: 'translateY(-100%)',
  },
  to: {
    transform: 'translateY(0)',
  },
})
export const topClose = keyframes({
  from: {
    transform: 'translateY(0)',
  },
  to: {
    transform: 'translateY(-100%)',
  },
})
export const bottomOpen = keyframes({
  from: {
    transform: 'translateY(100%)',
  },
  to: {
    transform: 'translateY(0)',
  },
})
export const bottomClose = keyframes({
  from: {
    transform: 'translateY(0)',
  },
  to: {
    transform: 'translateY(100%)',
  },
})
export const rightOpen = keyframes({
  from: {
    transform: 'translateX(100%)',
  },
  to: {
    transform: 'translateX(0)',
  },
})
export const rightClose = keyframes({
  from: {
    transform: 'translateX(0)',
  },
  to: {
    transform: 'translateX(100%)',
  },
})
export const leftOpen = keyframes({
  from: {
    transform: 'translateX(-100%)',
  },
  to: {
    transform: 'translateX(0)',
  },
})
export const leftClose = keyframes({
  from: {
    transform: 'translateX(0)',
  },
  to: {
    transform: 'translateX(-100%)',
  },
})
