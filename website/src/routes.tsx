import type { RouteObject } from "react-router"
import { RootLayout } from "./layouts/RootLayout"
import { LandingPage } from "./pages/LandingPage"
import { DocRoute } from "./pages/DocRoute"

export const routes: RouteObject[] = [
  {
    path: "/",
    element: <RootLayout />,
    children: [
      { index: true, element: <LandingPage /> },
      { path: "*", element: <DocRoute /> },
    ],
  },
]
