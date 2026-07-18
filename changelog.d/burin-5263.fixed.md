- **ACP prompt errors now carry Harn's typed terminal class.** Failed
  `session/prompt` responses preserve structured VM error categories in a
  versioned `error.data` contract, so hosts no longer need to classify rendered
  provider or runtime prose (burin-code#5263).
