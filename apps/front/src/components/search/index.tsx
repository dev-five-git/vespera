'use client'
import { Input } from '@devup-ui/components'
import { Box, css } from '@devup-ui/react'
import { ComponentProps } from 'react'

import { useSearchContext } from './provider'

export function Search(props: ComponentProps<typeof Input>) {
  const { value, setValue, setDimmed, insideClickRefs } = useSearchContext()
  return (
    <Input
      ref={(el) => {
        if (!el) return
        insideClickRefs.current.add(el)
        return () => {
          insideClickRefs.current.delete(el)
        }
      }}
      classNames={{
        container: css({ flex: '1' }),
        input: css({
          w: '100%',
          pl: '48px',
          py: '8px',
          pr: '16px',
          selectors: {
            '&:focus': {
              backgroundColor: 'var(--vesperaBg)',
            },
          },
        }),
      }}
      colors={{
        primary: 'var(--vesperaPrimary)',
        primaryFocus: 'var(--vesperaPrimary)',
        border: 'var(--border)',
      }}
      icon={
        <Box
          aspectRatio="1"
          bg="$border"
          boxSize="20px"
          maskImage="url(/icons/search.svg)"
          maskPos="center"
          maskRepeat="no-repeat"
          maskSize="contain"
        />
      }
      name="search"
      onChange={(e) => setValue(e.target.value)}
      onClick={() => setDimmed(true)}
      placeholder="Search documentation"
      typography="caption"
      value={value}
      {...props}
    />
  )
}
