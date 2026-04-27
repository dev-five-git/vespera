import { Flex } from '@devup-ui/react'
import { ComponentProps } from 'react'

export function Effect(props: ComponentProps<typeof Flex<'div'>>) {
  return (
    <Flex
      _active={{
        bg: '$border',
      }}
      _hover={{
        bg: '$cardBase',
      }}
      alignItems="center"
      borderRadius="100px"
      cursor="pointer"
      p="10px"
      styleOrder={1}
      transition="all .1s"
      {...props}
    />
  )
}
