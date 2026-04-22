'use client'

import { createContext, useContext, useState } from 'react'

import { useSheet } from '../sheet'

const HeaderContext = createContext<{
  menuOpen: boolean
  setMenuOpen: (menuOpen: boolean) => void
  transparent: boolean
} | null>(null)

export function useHeader() {
  const context = useContext(HeaderContext)
  if (!context) {
    throw new Error('useHeader must be used within a HeaderProvider')
  }
  return context
}

export function HeaderProvider({ children }: { children: React.ReactNode }) {
  const { isOpen } = useSheet()
  const [menuOpen, setMenuOpen] = useState(false)
  const transparent = !isOpen
  return (
    <HeaderContext.Provider value={{ menuOpen, setMenuOpen, transparent }}>
      {children}
    </HeaderContext.Provider>
  )
}
