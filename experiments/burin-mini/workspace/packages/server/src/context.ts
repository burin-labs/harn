export type DemoUser = {
  id: string
  name: string
  plan: string
}

export type AppContext = {
  headers: Record<string, string>
  state: {
    user: DemoUser | null
  }
}
