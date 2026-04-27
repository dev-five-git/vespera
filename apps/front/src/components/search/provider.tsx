'use client'

import {
  createContext,
  RefObject,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react'

const SearchContext = createContext<{
  value: string
  debouncedValue: string
  setValue: (value: string) => void
  dimmed: boolean
  setDimmed: (dimmed: boolean) => void
  insideClickRefs: RefObject<Set<HTMLElement>>
} | null>(null)

const DEBOUNCE_MS = 750

export function useSearchContext() {
  const context = useContext(SearchContext)
  if (!context) {
    throw new Error('useSearchContext must be used within a SearchProvider')
  }
  return context
}

export function Provider({ children }: { children: React.ReactNode }) {
  const [value, setValue] = useState('')
  const [debouncedValue, setDebouncedValue] = useState('')
  const [dimmed, setDimmed] = useState(false)
  const insideClickRefs = useRef<Set<HTMLElement>>(new Set())

  useEffect(() => {
    const id = setTimeout(() => setDebouncedValue(value), DEBOUNCE_MS)
    return () => clearTimeout(id)
  }, [value])

  useEffect(() => {
    function handleOutsideClick(event: MouseEvent) {
      if (
        Array.from(insideClickRefs.current).some((el) =>
          el.contains(event.target as HTMLElement),
        )
      ) {
        return
      }
      setDimmed(false)
    }
    document.addEventListener('click', handleOutsideClick)
    return () => {
      document.removeEventListener('click', handleOutsideClick)
    }
  }, [])

  useEffect(() => {
    if (!dimmed) return
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        setDimmed(false)
        insideClickRefs.current.forEach((el) => {
          el.blur()
        })
      }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => {
      document.removeEventListener('keydown', handleKeyDown)
    }
  }, [dimmed])

  return (
    <SearchContext.Provider
      value={{
        value,
        debouncedValue,
        setValue,
        dimmed,
        setDimmed,
        insideClickRefs,
      }}
    >
      {children}
    </SearchContext.Provider>
  )
}

export function SearchContextBoundary({
  state,
  children,
  reverse = false,
}: {
  state: 'value' | 'dimmed'
  reverse?: boolean
  children: React.ReactNode
}) {
  const { value, dimmed } = useSearchContext()
  const pass = state === 'value' ? !!value : !!dimmed
  if (reverse) return pass ? null : children
  return pass ? children : null
}

export { Provider as SearchProvider }
