import { Center, Text } from '@devup-ui/react'

export function Menu({ children }: { children?: React.ReactNode }) {
  return (
    <Center cursor="pointer" px="$spacingSpacing24" py="$spacingSpacing08">
      <Text
        // selected: '$vesperaPrimary',
        _hover={{
          color: '$textSub',
        }}
        color="$menutext"
        transition="all .1s"
        typography="menu"
        whiteSpace="nowrap"
      >
        {children}
      </Text>
    </Center>
  )
}
