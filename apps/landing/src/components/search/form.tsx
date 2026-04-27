'use client'
import { Box } from '@devup-ui/react'
import { useRouter } from 'next/navigation'
import { ComponentProps } from 'react'

export function Form(props: ComponentProps<typeof Box<'form'>>) {
  const router = useRouter()
  return (
    <Box
      as="form"
      display="contents"
      onSubmit={(e) => {
        e.preventDefault()
        const formData = new FormData(e.target as HTMLFormElement)
        const search = formData.get('search')
        if (search) router.push(`?search=${search}`)
      }}
      {...props}
    />
  )
}

export { Form as SearchForm }
