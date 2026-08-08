module Fixture where

data Color = Red | Green | Blue

newtype Counter = Counter Int

class Speaker a where
  speak :: a -> String

greet :: String -> String
greet name = "hello " ++ name

shout :: String -> String
shout = map toUpper
