import { Image } from '@devup-ui/react'
import { Box, Button, Center, Flex, Text, VStack } from '@devup-ui/react'

import { GnbIcon } from '@/components/header/gnb-icon'

export interface MenuProps {
  state: 'default' | 'hover' | 'selected'
}
export function Menu({ state }: MenuProps) {
  return (
    <Center px="$spacingSpacing24" py="$spacingSpacing08">
      <Text
        color={
          {
            default: '$menutext',
            hover: '$textSub',
            selected: '$vesperaPrimary',
          }[state]
        }
        typography="menu"
      >
        menu
      </Text>
    </Center>
  )
}
export interface SearchProps {
  clearButton?: boolean
  state: 'default' | 'hover' | 'selected'
}
export function Search({ clearButton, state }: SearchProps) {
  return (
    <Flex
      alignItems="center"
      border={
        {
          default: 'solid 1px $border',
          hover: 'solid 1px $caption',
          selected: 'solid 1px $vesperaPrimary',
        }[state]
      }
      borderRadius="$spacingSpacing08"
      gap="$spacingSpacing12"
      px="$spacingSpacing16"
      py="$spacingSpacing08"
    >
      <Text
        color={
          {
            default: '$border',
            hover: '$caption',
            selected: '$caption',
          }[state]
        }
        flex="1"
        typography="caption"
      >
        Search documentation
      </Text>
    </Flex>
  )
}
export interface EffectProps {
  state: 'deafult' | 'hover' | 'active'
  children: React.ReactNode
}
export function Effect({ children, state }: EffectProps) {
  return (
    <Flex
      alignItems="center"
      bg={
        {
          hover: '$cardBase',
          active: '$border',
        }[state]
      }
      borderRadius="100px"
      p="10px"
    >
      {children}
    </Flex>
  )
}

export default function HomePage() {
  return (
    <Box bg="#0A0E1A" color="#FFF" minH="100vh">
      <Center
        bg="url(/images/hero.webp) center/cover no-repeat"
        flexDir="column"
        h="1080px"
        overflow="hidden"
        pb="60px"
        pos="relative"
        pt="128px"
        px="40px"
      >
        <Box left="0px" pos="absolute" top="0px" w="100%">
          <Center
            backdropFilter="blur(20px)"
            bg="#FFFFFF03"
            flexDir="column"
            pl={['16px', null, null, null, 'initial']}
            pr={['4px', null, null, null, 'initial']}
            py="4px"
          >
            <Flex
              alignItems="center"
              justifyContent="space-between"
              maxW="1440px"
              w="100%"
            >
              <Center
                aspectRatio="5.09"
                flexDir={[null, null, null, null, 'row']}
                gap={[null, null, null, null, '16px']}
                h={['25px', null, null, null, 'initial']}
                w={['128px', null, null, null, 'initial']}
              >
                <Image h="28px" src="/icons/logo-image.svg" w="21px" />
                <Flex alignItems="center">
                  <Menu state="default" />
                  <Menu state="default" />
                </Flex>
              </Center>
              <Flex alignItems="center" gap="$spacingSpacing24">
                <Flex alignItems="center">
                  <Effect state="deafult">
                    <GnbIcon icon="github" />
                  </Effect>
                  <Effect state="deafult">
                    <GnbIcon icon="discord" />
                  </Effect>
                  <Effect state="deafult">
                    <GnbIcon icon="kakao" />
                  </Effect>
                  <Effect state="deafult">
                    <GnbIcon icon="theme-light" />
                  </Effect>
                </Flex>
              </Flex>
              <Flex
                alignItems="center"
                display={['flex', null, null, null, 'none']}
                justifyContent="flex-end"
                w="150px"
              ></Flex>
            </Flex>
          </Center>
        </Box>
        <Box
          bg="url(/icons/image.png) center/cover no-repeat"
          bottom="2px"
          display="none"
          h="100%"
          left="1074px"
          mixBlendMode="overlay"
          pos="absolute"
        />
        <VStack
          alignItems="center"
          gap="$spacingSpacing64"
          maxW="1280px"
          w="100%"
        >
          <VStack alignItems="center" gap="$spacingSpacing32" w="100%">
            <Text color="$title" textAlign="center" typography="displaySm">
              Lorem ipsum dolor sit amet, <br />
              consectetur adipiscing elit.
            </Text>
            <Text color="$title" textAlign="center" typography="title">
              Etiam sit amet feugiat turpis. Proin nec ante a sem vestibulum
              sodales non ut ex. <br />
              Morbi diam turpis, fringilla vitae enim et, egestas consequat
              nibh. <br />
              Etiam auctor cursus urna sit amet elementum.
            </Text>
          </VStack>
          <Button />
        </VStack>
      </Center>

      <Box bg="#FFF" color="#10131F" py={[16, null, 24]}>
        <Box maxW="1200px" mx="auto" px={[4, null, 8]}>
          <Text
            as="h2"
            color="#10131F"
            fontSize={['28px', null, '40px']}
            fontWeight={700}
            letterSpacing="-0.01em"
            mb={4}
          >
            Title
          </Text>
          <Text
            color="#4B5263"
            fontSize={['15px', null, '16px']}
            lineHeight={1.6}
            maxW="760px"
            mb={12}
          >
            Lorem ipsum dolor sit amet. Etiam sit amet feugiat turpis. Proin nec
            ante a sem vestibulum sodales non ut ex. Lorem ipsum dolor sit amet,
            consectetur adipiscing elit.
          </Text>

          <Flex flexWrap="wrap" gap={5}>
            {[0, 1, 2, 3].map((i) => (
              <Box
                key={i}
                _hover={{
                  borderColor: '#377DFF',
                  transform: 'translateY(-2px)',
                }}
                bg="#F3F4F6"
                border="1px solid transparent"
                borderRadius="14px"
                flex="1 1 240px"
                minW="240px"
                p={6}
                transition="all .2s"
              >
                <Box
                  bg="linear-gradient(135deg,#377DFF 0%,#003EA0 100%)"
                  borderRadius="10px"
                  boxSize="44px"
                  mb={5}
                />
                <Text color="#10131F" fontSize="17px" fontWeight={700} mb={2}>
                  Feature title
                </Text>
                <Text color="#4B5263" fontSize="14px" lineHeight={1.6}>
                  Lorem ipsum dolor sit amet. Etiam sit amet feugiat turpis.
                  Proin nec ante a sem vestibulum sodales non ut ex.
                </Text>
              </Box>
            ))}
          </Flex>
        </Box>
      </Box>

      <Box py={[16, null, 24]}>
        <Box maxW="1200px" mx="auto" px={[4, null, 8]}>
          <Text
            as="h2"
            color="#FFF"
            fontSize={['28px', null, '40px']}
            fontWeight={700}
            letterSpacing="-0.01em"
            mb={4}
          >
            How it works
          </Text>
          <Text
            color="#B6B6B6"
            fontSize={['15px', null, '16px']}
            lineHeight={1.6}
            maxW="760px"
            mb={12}
          >
            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Nullam
            venenatis ac egestas lacus est nec urna.
          </Text>

          <Flex alignItems="stretch" flexDir={['column', null, 'row']} gap={8}>
            <VStack flex={1} gap={6}>
              <Box
                bg="rgba(255,255,255,0.03)"
                border="1px solid rgba(255,255,255,0.08)"
                borderRadius="14px"
                p={6}
              >
                <Text color="#FFF" fontSize="18px" fontWeight={700} mb={2}>
                  작동 방법
                </Text>
                <Text color="#B6B6B6" fontSize="14px" lineHeight={1.6} mb={4}>
                  Lorem ipsum dolor sit amet. Etiam sit amet feugiat turpis.
                  Proin nec ante a sem vestibulum sodales non ut ex.
                </Text>
                <Text
                  _hover={{ color: '#FFF' }}
                  as="a"
                  color="#377DFF"
                  cursor="pointer"
                  fontSize="14px"
                  fontWeight={600}
                >
                  Learn more →
                </Text>
              </Box>

              <Flex flexDir={['column', null, 'row']} gap={6} w="100%">
                <Box
                  bg="rgba(255,255,255,0.03)"
                  border="1px solid rgba(255,255,255,0.08)"
                  borderRadius="14px"
                  flex={1}
                  p={6}
                >
                  <Text color="#FFF" fontSize="16px" fontWeight={700} mb={2}>
                    예시
                  </Text>
                  <Text color="#B6B6B6" fontSize="13px" lineHeight={1.6}>
                    Lorem ipsum dolor sit amet. Etiam sit amet feugiat turpis.
                  </Text>
                </Box>
                <Box
                  bg="rgba(255,255,255,0.03)"
                  border="1px solid rgba(255,255,255,0.08)"
                  borderRadius="14px"
                  flex={1}
                  p={6}
                >
                  <Text color="#FFF" fontSize="16px" fontWeight={700} mb={2}>
                    성과
                  </Text>
                  <Text color="#B6B6B6" fontSize="13px" lineHeight={1.6}>
                    Lorem ipsum dolor sit amet. Etiam sit amet feugiat turpis.
                  </Text>
                </Box>
              </Flex>
            </VStack>

            <Box
              bg="#0D1220"
              border="1px solid rgba(255,255,255,0.08)"
              borderRadius="14px"
              flex={1}
              overflow="hidden"
            >
              <Flex
                alignItems="center"
                borderBottom="1px solid rgba(255,255,255,0.08)"
                gap={2}
                px={4}
                py={3}
              >
                <Box bg="#FF5F57" borderRadius="50%" boxSize="12px" />
                <Box bg="#FEBC2E" borderRadius="50%" boxSize="12px" />
                <Box bg="#28C840" borderRadius="50%" boxSize="12px" />
                <Text color="#626770" fontSize="12px" ml={3}>
                  example.tsx
                </Text>
              </Flex>
              <Box
                as="pre"
                color="#B6B6B6"
                fontFamily="ui-monospace, 'SF Mono', Menlo, monospace"
                fontSize="13px"
                lineHeight={1.7}
                overflow="auto"
                p={5}
              >
                <Box as="code">
                  <Box as="span" color="#C792EA">
                    import
                  </Box>{' '}
                  <Box as="span" color="#82AAFF">
                    {'{ Box }'}
                  </Box>{' '}
                  <Box as="span" color="#C792EA">
                    from
                  </Box>{' '}
                  <Box as="span" color="#C3E88D">
                    &apos;@devup-ui/react&apos;
                  </Box>
                  {'\n\n'}
                  <Box as="span" color="#C792EA">
                    export default function
                  </Box>{' '}
                  <Box as="span" color="#82AAFF">
                    Page
                  </Box>
                  <Box as="span" color="#89DDFF">
                    () {'{'}
                  </Box>
                  {'\n  '}
                  <Box as="span" color="#C792EA">
                    return
                  </Box>{' '}
                  <Box as="span" color="#89DDFF">
                    {'<'}
                  </Box>
                  <Box as="span" color="#F07178">
                    Box
                  </Box>{' '}
                  <Box as="span" color="#FFCB6B">
                    bg
                  </Box>
                  <Box as="span" color="#89DDFF">
                    =
                  </Box>
                  <Box as="span" color="#C3E88D">
                    &quot;$primary&quot;
                  </Box>{' '}
                  <Box as="span" color="#FFCB6B">
                    p
                  </Box>
                  <Box as="span" color="#89DDFF">
                    =
                  </Box>
                  <Box as="span" color="#F78C6C">
                    {'{4}'}
                  </Box>
                  <Box as="span" color="#89DDFF">
                    {'>'}
                  </Box>
                  {'\n    Hello Vespera\n  '}
                  <Box as="span" color="#89DDFF">
                    {'</'}
                  </Box>
                  <Box as="span" color="#F07178">
                    Box
                  </Box>
                  <Box as="span" color="#89DDFF">
                    {'>'}
                  </Box>
                  {'\n'}
                  <Box as="span" color="#89DDFF">
                    {'}'}
                  </Box>
                </Box>
              </Box>
            </Box>
          </Flex>
        </Box>
      </Box>

      <Box overflow="hidden" pos="relative" py={[16, null, 24]}>
        <Box
          bg="radial-gradient(circle, #377DFF 0%, transparent 70%)"
          filter="blur(80px)"
          h="500px"
          left="-10%"
          opacity={0.3}
          pos="absolute"
          top="50%"
          transform="translateY(-50%)"
          w="500px"
        />

        <Box maxW="1200px" mx="auto" pos="relative" px={[4, null, 8]}>
          <Flex
            alignItems="center"
            bg="rgba(255,255,255,0.03)"
            border="1px solid rgba(255,255,255,0.08)"
            borderRadius="20px"
            flexDir={['column', null, 'row']}
            gap={10}
            overflow="hidden"
            p={[8, null, 12]}
            pos="relative"
          >
            <Box flexShrink={0} h="240px" pos="relative" w="240px">
              <Box
                border="1px solid rgba(255,255,255,0.08)"
                borderRadius="50%"
                boxSize="240px"
                left={0}
                pos="absolute"
                top={0}
              />
              <Box
                border="1px solid rgba(255,255,255,0.12)"
                borderRadius="50%"
                boxSize="170px"
                left="35px"
                pos="absolute"
                top="35px"
              />
              <Box
                border="1px solid rgba(255,255,255,0.18)"
                borderRadius="50%"
                boxSize="100px"
                left="70px"
                pos="absolute"
                top="70px"
              />
              <Box
                bg="radial-gradient(circle at 30% 30%, #88B5FF 0%, #377DFF 60%, #003EA0 100%)"
                borderRadius="50%"
                boxShadow="0 0 40px rgba(55,125,255,0.6)"
                boxSize="44px"
                left="98px"
                pos="absolute"
                top="98px"
              />
              <Box
                bg="#377DFF"
                borderRadius="50%"
                boxShadow="0 0 16px rgba(55,125,255,0.8)"
                boxSize="14px"
                left="226px"
                pos="absolute"
                top="113px"
              />
              <Box
                bg="#82AAFF"
                borderRadius="50%"
                boxShadow="0 0 12px rgba(130,170,255,0.6)"
                boxSize="10px"
                left="30px"
                pos="absolute"
                top="40px"
              />
              <Box
                bg="#FFF"
                borderRadius="50%"
                boxSize="6px"
                left="200px"
                opacity={0.6}
                pos="absolute"
                top="190px"
              />
            </Box>

            <Box flex={1} textAlign={['center', null, 'left']}>
              <Text
                as="h2"
                color="#FFF"
                fontSize={['24px', null, '32px']}
                fontWeight={700}
                letterSpacing="-0.01em"
                mb={3}
              >
                Join our community
              </Text>
              <Text
                color="#B6B6B6"
                fontSize={['15px', null, '16px']}
                lineHeight={1.6}
                mb={6}
              >
                Join our Discord and help build the future of frontend with
                CSS-in-JS!
              </Text>
              <Flex gap={3} justifyContent={['center', null, 'flex-start']}>
                <Box
                  _hover={{ bg: '#2960CC' }}
                  as="a"
                  bg="#377DFF"
                  borderRadius="10px"
                  color="#FFF"
                  cursor="pointer"
                  fontSize="14px"
                  fontWeight={600}
                  px={5}
                  py="10px"
                  transition="background .2s"
                >
                  Discord
                </Box>
                <Box
                  _hover={{ bg: 'rgba(255,255,255,0.06)' }}
                  as="a"
                  border="1px solid rgba(255,255,255,0.2)"
                  borderRadius="10px"
                  color="#FFF"
                  cursor="pointer"
                  fontSize="14px"
                  fontWeight={600}
                  px={5}
                  py="10px"
                  transition="background .2s"
                >
                  GitHub
                </Box>
              </Flex>
            </Box>
          </Flex>
        </Box>
      </Box>

      <Box as="footer" borderTop="1px solid rgba(255,255,255,0.08)" py={10}>
        <Box maxW="1200px" mx="auto" px={[4, null, 8]}>
          <Flex
            alignItems={['flex-start', null, 'center']}
            flexDir={['column', null, 'row']}
            gap={6}
            justifyContent="space-between"
          >
            <VStack alignItems="flex-start" gap={2}>
              <Flex alignItems="center" gap={2}>
                <Box
                  bg="linear-gradient(135deg,#377DFF 0%,#003EA0 100%)"
                  borderRadius="6px"
                  boxSize="24px"
                />
                <Text color="#FFF" fontSize="16px" fontWeight={700}>
                  DEVFIVE
                </Text>
              </Flex>
              <Text color="#626770" fontSize="13px">
                Copyright © 데브파이브. All Rights Reserved.
              </Text>
            </VStack>
            <VStack alignItems={['flex-start', null, 'flex-end']} gap={1}>
              <Text color="#B6B6B6" fontSize="13px">
                문의 및 의견 제출
              </Text>
              <Text
                _hover={{ color: '#FFF' }}
                as="a"
                color="#377DFF"
                cursor="pointer"
                fontSize="14px"
                fontWeight={600}
              >
                contact@devfive.kr
              </Text>
            </VStack>
          </Flex>
        </Box>
      </Box>
    </Box>
  )
}
