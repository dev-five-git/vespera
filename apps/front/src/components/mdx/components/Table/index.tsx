import { Box, Grid } from '@devup-ui/react'
import { type ComponentProps } from 'react'

export const Table = ({ ...props }: ComponentProps<'table'>) => {
  return (
    <Grid>
      <Box maxW="100%" overflow="auto" w="100%">
        <Box
          as="table"
          borderCollapse="collapse"
          borderSpacing={0}
          whiteSpace="nowrap"
          {...props}
        />
      </Box>
    </Grid>
  )
}

export const TableBody = ({ ...props }: ComponentProps<'tbody'>) => {
  return <Box {...props} as="tbody" />
}

export const TableCell = ({ ...props }: ComponentProps<'th'>) => {
  return <Box {...props} as="td" padding="0.5rem 1rem" />
}

export const TableHead = ({ ...props }: ComponentProps<'thead'>) => {
  return (
    <Box
      {...props}
      as="thead"
      selectors={{
        '& tr': {
          bg: '$cardBg',
        },
      }}
    />
  )
}

export const TableHeaderCell = ({ ...props }: ComponentProps<'th'>) => {
  return <Box {...props} as="th" padding="0.5rem 1rem" textAlign="left" />
}

export const TableRow = ({ ...props }: ComponentProps<'tr'>) => {
  return (
    <Box
      {...props}
      as="tr"
      borderBottom="1px solid var(--border, #E4E4E4)"
      selectors={{
        '& + &:last-of-type': {
          borderBottom: 'none',
        },
      }}
    />
  )
}
