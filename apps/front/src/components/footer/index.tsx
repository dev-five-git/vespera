import { Box, Flex, Text, VStack } from '@devup-ui/react'

export function Footer() {
  return (
    <Flex
      bg="#2F313B"
      gap={['20px', null, null, 'initial']}
      justifyContent={[null, null, null, 'center']}
      px={['20px', null, null, 'initial']}
      py={['30px', null, null, '60px']}
    >
      <VStack
        flex="1"
        flexDir={['column-reverse', null, null, 'row']}
        gap="20px"
        maxW="1280px"
        px={[null, null, null, '30px']}
        w="100%"
      >
        <VStack gap="20px">
          <Text color="#FFF" typography="footerBold" wordBreak="keep-all">
            문의 및 의견 제출
            <br />
            contact@devfive.kr
          </Text>
          <Text color="#CACACA" typography="footerCopy">
            Copyright © DEVFIVE. All Rights Reserved.{' '}
          </Text>
        </VStack>
        <Flex
          flex="1"
          gap="4px"
          justifyContent={['flex-start', null, null, 'flex-end']}
        >
          <Text color="#FFF" typography="footerBold">
            DEVFIVE{' '}
          </Text>
          <Box
            bg="$base"
            boxSize="24px"
            maskImage="url('/icons/external-link.svg')"
            maskPos="center"
            maskRepeat="no-repeat"
            maskSize="contain"
          />
        </Flex>
      </VStack>
    </Flex>
  )
}
