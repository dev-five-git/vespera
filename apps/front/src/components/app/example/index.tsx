'use client'

import { Flex, Image } from '@devup-ui/react'
import { ComponentProps, createContext, useContext, useState } from 'react'

const ExampleContext = createContext<{
  selected: string
  setSelected: (selected: string) => void
  selectedExample?: {
    id: string
    title: string
    description: string
    imageUrl: string
  }
} | null>(null)

export function useExample() {
  const context = useContext(ExampleContext)
  if (!context) {
    throw new Error('useExample must be used within a ExampleProvider')
  }
  return context
}

export function ExampleProvider({
  defaultSelected = '',
  examples,
  children,
}: {
  defaultSelected?: string
  examples: {
    id: string
    title: string
    description: string
    imageUrl: string
  }[]
  children: React.ReactNode
}) {
  const [selected, setSelected] = useState(defaultSelected)
  const selectedExample = examples.find((example) => example.id === selected)
  return (
    <ExampleContext.Provider
      value={{
        selected,
        setSelected,
        selectedExample,
      }}
    >
      {children}
    </ExampleContext.Provider>
  )
}

export function ExampleContainer({
  value,
  ...props
}: ComponentProps<typeof Flex<'div'>> & { value?: string }) {
  const { selected, setSelected } = useExample()
  const isSelected = selected === value
  return (
    <Flex
      _hover={{
        bg: '#1F2737',
      }}
      bg={
        isSelected
          ? 'linear-gradient(90deg, #161A2A 0%, #121F33 100%)'
          : 'transparent'
      }
      borderColor={isSelected ? '$caption' : 'transparent'}
      borderRadius="$spacingSpacing08"
      borderStyle="solid"
      borderWidth="2px"
      onClick={value ? () => setSelected(value) : undefined}
      overflow="hidden"
      px="$spacingSpacing24"
      py="$spacingSpacing20"
      styleOrder={1}
      transition="all .1s"
      {...props}
    />
  )
}

export function ExampleImage({
  ...props
}: Omit<ComponentProps<typeof Image<'img'>>, 'src'>) {
  const { selectedExample } = useExample()
  return (
    <Image
      alt={selectedExample?.title ?? ''}
      bottom="0px"
      h="100%"
      left={['39px', null, null, '76px']}
      pos="absolute"
      right="0px"
      src={selectedExample?.imageUrl}
      styleOrder={1}
      top={['47px', null, null, '91px']}
      w="100%"
      {...props}
    />
  )
}

export function Example() {}
