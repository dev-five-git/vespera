'use client'
import { Center, Text } from '@devup-ui/react'
import { usePathname } from 'next/navigation'

export function Menu({
  value,
  children,
}: {
  value: string
  children?: React.ReactNode
}) {
  const pathname = usePathname()
  const isSelected = pathname.includes(value)

  return (
    <Center cursor="pointer" px="$spacingSpacing24" py="$spacingSpacing08">
      <Text
        _hover={{
          color: '$textSub',
        }}
        color={isSelected ? '$vesperaPrimary' : '$menutext'}
        transition="all .1s"
        typography="menu"
        whiteSpace="nowrap"
      >
        {children}
      </Text>
    </Center>
  )
}
