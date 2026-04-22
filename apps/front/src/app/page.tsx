import { Box, Center, css, Flex, Text, VStack } from '@devup-ui/react'
import { Image } from '@devup-ui/react'

import { Button } from '@/components/button'
import { GnbIcon } from '@/components/header/gnb-icon'

export default function HomePage() {
  return (
    <>
      <Box bg="#0A0E1A" color="#FFF" minH="100vh">
        <Center
          bg="url(/images/hero.webp) center/cover no-repeat"
          flexDir="column"
          h="1080px"
          pb="60px"
          pos="relative"
          pt="128px"
          px="40px"
        >
          <Box
            bg="url(/icons/image.png) center/cover no-repeat"
            bottom="2px"
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
            <Button>Get started</Button>
          </VStack>
        </Center>

        <Center
          bg="$containerBackground"
          flexDir="column"
          overflow="hidden"
          px="20px"
          py="$spacingSpacing80"
        >
          <VStack gap="40px" maxW="1280px" w="100%">
            <VStack gap="16px">
              <Text color="$title" typography="h3">
                Title
              </Text>
              <Text color="$text" typography="body">
                Lorem ipsum dolor sit amet, consectetur adipiscing elit. Nullam
                venenatis, elit in hendrerit porta, augue ante scelerisque diam,{' '}
                <br />
                ac egestas lacus est nec urna. Cras commodo risus hendrerit,
                suscipit nibh at, porttitor dui.
              </Text>
            </VStack>
            <VStack flexDir={[null, null, null, 'row']} gap={5}>
              {[0, 1, 2, 3].map((i) => (
                <Flex
                  key={i}
                  bg="$cardBase"
                  borderRadius="$spacingSpacing08"
                  minH={['200px', null, null, '320px']}
                  overflow="hidden"
                  px={['$spacingSpacing20', null, null, '$spacingSpacing24']}
                  py={['$spacingSpacing16', null, null, '$spacingSpacing24']}
                >
                  <VStack
                    flex="1"
                    gap={['10px', null, null, '$spacingSpacing12']}
                  >
                    <Text color="$title" typography="title">
                      Feature title
                    </Text>
                    <Text color="$textSub" typography="body">
                      Lorem ipsum dolor sit amet. Etiam sit amet feugiat turpis.
                      Proin nec ante a sem vestibulum sodales non ut ex.{' '}
                    </Text>
                  </VStack>
                </Flex>
              ))}
            </VStack>
          </VStack>
        </Center>

        <Center
          bg="#10131F"
          flexDir="column"
          overflow="hidden"
          px="20px"
          py={['80px', null, null, '120px']}
        >
          <VStack gap="40px" maxW={[null, null, null, '1280px']} w="100%">
            <VStack gap="16px">
              <Text color="#FFF" typography="h3">
                Title
              </Text>
              <Text color="#FFF" typography="body">
                Lorem ipsum dolor sit amet, consectetur adipiscing elit. Nullam
                venenatis ac egestas lacus est nec urna.{' '}
              </Text>
            </VStack>
            <VStack
              alignItems="center"
              flexDir={[null, null, null, 'row-reverse']}
              gap="$spacingSpacing32"
              pos="relative"
            >
              <Flex
                bg="linear-gradient(90deg, #161A2A 0%, #121F33 100%)"
                borderRadius="$spacingSpacing08"
                flexShrink="0"
                h="424px"
                justifyContent="center"
                overflow="hidden"
                pos="relative"
                px="$spacingSpacing20"
                py="20px"
                w="624px"
              >
                <Image
                  left="76px"
                  pos="absolute"
                  right="0px"
                  src="/images/code.webp"
                  top="47px"
                  w="100%"
                />
                <Box
                  bottom="27px"
                  left="50%"
                  pos="absolute"
                  transform="translateX(-50%)"
                >
                  <Button>Learn more</Button>
                </Box>
              </Flex>
              <VStack gap="$spacingSpacing12" w="100%">
                {[1, 2, 3].map((i) => (
                  <Flex
                    key={i}
                    _hover={{
                      bg: '#1F2737',
                    }}
                    // selected:
                    //   'linear-gradient(90deg, #161A2A 0%, #121F33 100%)',
                    bg="linear-gradient(90deg, #161A2A 0%, #121F33 100%)"
                    border="solid 2px $caption"
                    borderRadius="$spacingSpacing08"
                    overflow="hidden"
                    px="$spacingSpacing24"
                    py="$spacingSpacing20"
                    transition="all .1s"
                  >
                    <VStack
                      flex="1"
                      gap={['10px', null, null, '$spacingSpacing12']}
                    >
                      <Text color="#FFF" typography="title">
                        How to Use
                      </Text>
                      <Text color="#FFF" typography="body">
                        Lorem ipsum dolor sit amet. Etiam sit amet feugiat
                        turpis. Proin nec ante a sem vestibulum sodales non ut
                        ex.{' '}
                      </Text>
                    </VStack>
                  </Flex>
                ))}
              </VStack>
            </VStack>
          </VStack>
        </Center>

        <VStack
          alignItems={[null, null, null, 'flex-end']}
          bg="#000"
          gap="40px"
          h={['600px', null, null, 'unset']}
          overflow="hidden"
          pos="relative"
          px={['20px', null, null, '40px']}
          py={['80px', null, null, '120px']}
        >
          <VStack
            gap="40px"
            justifyContent="center"
            maxW="480px"
            mr={[null, null, null, '210px']}
            w="100%"
            zIndex="10"
          >
            <VStack gap="16px">
              <Text color="#FFF" typography="h3">
                Join our community
              </Text>
              <Text color="#FFF" typography="body">
                Join our Discord and help build the future of frontend with
                CSS-in-JS!{' '}
              </Text>
            </VStack>
            <Flex alignItems="center" gap="16px">
              <Flex
                _active={{
                  bg: '#6B9FFF99',
                }}
                _hover={{
                  bg: '#6B9FFF66',
                }}
                alignItems="center"
                bg="#6B9FFF40"
                borderRadius="100px"
                p="16px"
              >
                <GnbIcon
                  className={css({ bg: '$vesperaPrimary' })}
                  icon="discord"
                />
              </Flex>
              <Flex
                _active={{
                  bg: '#6B9FFF99',
                }}
                _hover={{
                  bg: '#6B9FFF66',
                }}
                alignItems="center"
                bg="#6B9FFF40"
                borderRadius="100px"
                p="16px"
              >
                <GnbIcon
                  className={css({ bg: '$vesperaPrimary' })}
                  icon="kakao"
                />
              </Flex>
              <Flex
                _active={{
                  bg: '#6B9FFF99',
                }}
                _hover={{
                  bg: '#6B9FFF66',
                }}
                alignItems="center"
                bg="#6B9FFF40"
                borderRadius="100px"
                p="16px"
              >
                <GnbIcon
                  className={css({ bg: '$vesperaPrimary' })}
                  icon="devfive"
                />
              </Flex>
            </Flex>
          </VStack>
          <Image
            bottom="-225px"
            boxSize="500px"
            left="-122px"
            pos="absolute"
            src="/images/join-us-bg.webp"
          />
        </VStack>
      </Box>
    </>
  )
}
