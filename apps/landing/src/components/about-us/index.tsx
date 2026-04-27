import { Flex, Text, VStack } from '@devup-ui/react'

export function AboutUs() {
  return (
    <Flex
      _hover={{
        bg: '$cardBase',
        border: 'solid 1px $caption',
      }}
      alignItems="flex-end"
      bg="$containerBackground"
      border="solid 1px $border"
      borderRadius="$spacingSpacing08"
      cursor="pointer"
      justifyContent="space-between"
      overflow="hidden"
      px="24px"
      py="$spacingSpacing24"
      transition="all .1s"
    >
      <Flex flex="1">
        <VStack flex="1" gap="4px">
          <Text color="$title" typography="bodyLg">
            DEVFIVE
          </Text>
          <Text color="$caption" typography="buttonSm">
            https://devfive.kr/ko/
          </Text>
        </VStack>
      </Flex>
    </Flex>
  )
}
