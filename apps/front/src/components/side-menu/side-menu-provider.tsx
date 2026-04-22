'use client'

import { createContext, useContext, useState } from 'react'

const SideMenuContext = createContext<{
  selected: string | null
  setSelected: (selected: string | null) => void
} | null>(null)
export function useSideMenu() {
  const context = useContext(SideMenuContext)
  if (!context) {
    throw new Error('useSideMenu must be used within a SideMenuProvider')
  }
  return context
}
export function SideMenuProvider({
  defaultSelected = null,
  selected: controlledSelected,
  onSelect,
  children,
}: {
  defaultSelected?: string | null
  selected?: string | null
  onSelect?: (selected: string | null) => void
  children?: React.ReactNode
}) {
  const [innerSelected, setInnerSelected] = useState<string | null>(
    defaultSelected,
  )
  const handleSelect = (selected: string | null) => {
    setInnerSelected(selected)
    onSelect?.(selected)
  }
  const selected = controlledSelected ?? innerSelected
  return (
    <SideMenuContext.Provider value={{ selected, setSelected: handleSelect }}>
      {children}
    </SideMenuContext.Provider>
  )
}
