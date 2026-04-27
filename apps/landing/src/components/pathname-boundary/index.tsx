'use client'

import { usePathname } from 'next/navigation'

export function PathnameBoundary({
  children,
  reverse = false,
  candidates = [],
}: {
  candidates: string[]
  children?: React.ReactNode
  reverse?: boolean
}) {
  const pathname = usePathname()
  const pass = candidates.includes(pathname)
  if (reverse) return pass ? null : children
  return pass ? children : null
}
