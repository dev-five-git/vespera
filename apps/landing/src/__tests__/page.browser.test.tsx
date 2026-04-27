import { ThemeScript } from '@devup-ui/react'
import { describe, expect, it } from 'bun:test'
import { render } from 'bun-test-env-dom'

import HomePage from '@/app/page'

describe('HomePage', () => {
  it('should render', () => {
    const { container } = render(<ThemeScript />)
    expect(container).toMatchSnapshot()
    expect(<HomePage />).toMatchSnapshot()
  })
})
