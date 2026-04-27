import { Box, Flex, Text } from '@devup-ui/react'

export function Edit() {
  return (
    <Flex
      alignItems="center"
      borderRadius="$spacingSpacing08"
      cursor="pointer"
      gap="$spacingSpacing08"
      py="$spacingSpacing08"
      role="group"
    >
      <Text
        _groupHover={{
          color: '$textSub',
        }}
        color="$caption"
        typography="tiny"
      >
        Edit this page
      </Text>
      <Box
        _groupHover={{
          bg: '$textSub',
        }}
        aspectRatio="1"
        bg="$caption"
        boxSize="16px"
        maskImage="url(/icons/arrow-up-right.svg)"
        maskPos="center"
        maskRepeat="no-repeat"
        maskSize="contain"
      />
    </Flex>
  )
}
