import { css } from '@devup-ui/react'

import { SheetContainer } from '../sheet'

export function MobileMenu() {
  return (
    <SheetContainer
      className={css({
        borderRadius: '0px',
        top: '68px',
      })}
      position="right"
    ></SheetContainer>
  )
}
