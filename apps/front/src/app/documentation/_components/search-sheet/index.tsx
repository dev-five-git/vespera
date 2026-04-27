import { Box, css } from '@devup-ui/react'

import { Effect } from '@/components/header/effect'
import { Search as SearchComponent } from '@/components/search'
import { SearchForm } from '@/components/search/form'
import { SheetRouteTrigger } from '@/components/sheet/router'

import { SearchSheetRouteContainer } from './route-container'

export function SearchSheet() {
  return (
    <SearchSheetRouteContainer
      className={css({
        display: 'flex',
        alignItems: 'center',
        borderRadius: '0px',
        h: '68px',
        py: '$spacingSpacing12',
        pl: '$spacingSpacing04',
        pr: '$spacingSpacing20',
        gap: '4px',
        zIndex: '200',
      })}
      name="search"
      position="top"
    >
      <SheetRouteTrigger name="search">
        <Effect>
          <Box
            aspectRatio="1"
            bg="$title"
            boxSize="24px"
            maskImage="url('/icons/close.svg')"
            maskPos="center"
            maskRepeat="no-repeat"
            maskSize="contain"
          />
        </Effect>
      </SheetRouteTrigger>
      <SearchForm>
        <SearchComponent id="mobile-search" />
      </SearchForm>
    </SearchSheetRouteContainer>
  )
}
