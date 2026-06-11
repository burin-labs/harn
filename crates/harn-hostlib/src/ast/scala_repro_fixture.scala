package rulekit

/** Serialises and deserialises Rules to/from a simple JSON format.
  *
  * Supported rule types: Predicate, AllOf, AnyOf, NoneOf, Custom.
  * Supported condition types: Equals, NotEquals, GreaterThan,
  * GreaterThanOrEqual, LessThan, LessThanOrEqual, Between, Contains,
  * StartsWith, EndsWith, Matches, IsNull, IsNotNull, OneOf, IsEmpty,
  * IsNotEmpty, Custom.
  *
  * JSON format:
  *   - Predicate: { "type": "Predicate", "name": "...", "condition": { ... } }
  *   - AllOf:     { "type": "AllOf",     "name": "...", "rules": [ ... ] }
  *   - AnyOf:     { "type": "AnyOf",     "name": "...", "rules": [ ... ] }
  *   - NoneOf:    { "type": "NoneOf",    "name": "...", "rules": [ ... ] }
  *   - Custom:    { "type": "Custom",    "name": "..." }
  *
  * Condition format: { "type": "Equals", "field": "...", "expected": ... }
  */
object RuleSerializer:

  // -- JSON value types (minimal, no external dependency) --

  sealed trait JsonValue
  case class JObject(fields: List[(String, JsonValue)]) extends JsonValue
  case class JArray(elements: List[JsonValue]) extends JsonValue
  case class JString(value: String) extends JsonValue
  case class JNumber(value: Double) extends JsonValue
  case class JBoolean(value: Boolean) extends JsonValue
  case object JNull extends JsonValue

  // -- Simple JSON parser --

  object JsonParser:

    def parse(input: String): JsonValue =
      val tokens = tokenize(input)
      val (result, _) = parseValue(tokens, 0)
      result

    private def tokenize(input: String): List[String] =
      val sb = new StringBuilder
      val tokens = scala.collection.mutable.ListBuffer[String]()
      var inString = false
      var escape = false
      var i = 0
      while i < input.length do
        val c = input.charAt(i)
        if inString then
          if escape then
            sb.append(c)
            escape = false
          else if c == '\\' then
            escape = true
          else if c == '"' then
            tokens += sb.toString()
            sb.clear()
            inString = false
          else
            sb.append(c)
        else
          if c == '"' then
            inString = true
          else if " \t\n\r,".contains(c) then
            if sb.nonEmpty then
              tokens += sb.toString()
              sb.clear()
          else if "[]{}:,".contains(c) then
            if sb.nonEmpty then
              tokens += sb.toString()
              sb.clear()
            tokens += c.toString()
          else
            sb.append(c)
        i += 1
      if sb.nonEmpty then tokens += sb.toString()
      tokens.toList

    private def parseValue(tokens: List[String], pos: Int): (JsonValue, Int) =
      tokens(pos) match
        case "null"   => (JNull, pos + 1)
        case "true"   => (JBoolean(true), pos + 1)
        case "false"  => (JBoolean(false), pos + 1)
        case s if s.startsWith("\"") && s.endsWith("\"") =>
          (JString(s.substring(1, s.length - 1)), pos + 1)
        case s =>
          try
            (JNumber(s.toDouble), pos + 1)
          catch
            case _: NumberFormatException =>
              (JString(s), pos + 1)

    private def parseObject(tokens: List[String], pos: Int): (JObject, Int) =
      var current = pos + 1 // skip {
      val fields = scala.collection.mutable.ListBuffer[(String, JsonValue)]()
      if current < tokens.length && tokens(current) == "}" then
        return (JObject(fields.toList), current + 1)
      while current < tokens.length do
        val (key, next) = parseValue(tokens, current)
        val (colon, next2) = parseValue(tokens, next)
        val (value, next3) = parseValue(tokens, next2)
        key match
          case JString(k) => fields += ((k, value))
          case _ =>
        current = next3
        if current < tokens.length && tokens(current) == "," then
          current += 1
        else if current < tokens.length && tokens(current) == "}" then
          return (JObject(fields.toList), current + 1)
      (JObject(fields.toList), current)

    private def parseArray(tokens: List[String], pos: Int): (JArray, Int) =
      var current = pos + 1 // skip [
      val elements = scala.collection.mutable.ListBuffer[JsonValue]()
      if current < tokens.length && tokens(current) == "]" then
        return (JArray(elements.toList), current + 1)
      while current < tokens.length do
        val (elem, next) = parseValue(tokens, current)
        elements += elem
        current = next
        if current < tokens.length && tokens(current) == "," then
          current += 1
        else if current < tokens.length && tokens(current) == "]" then
          return (JArray(elements.toList), current + 1)
      (JArray(elements.toList), current)

    private def parseValue(tokens: List[String], pos: Int): (JsonValue, Int) =
      if pos >= tokens.length then return (JNull, pos)
      tokens(pos) match
        case "{" => parseObject(tokens, pos)
        case "[" => parseArray(tokens, pos)
        case _   => parseLeaf(tokens, pos)

    private def parseLeaf(tokens: List[String], pos: Int): (JsonValue, Int) =
      tokens(pos) match
        case "null"   => (JNull, pos + 1)
        case "true"   => (JBoolean(true), pos + 1)
        case "false"  => (JBoolean(false), pos + 1)
        case s if s.startsWith("\"") && s.endsWith("\"") =>
          (JString(s.substring(1, s.length - 1)), pos + 1)
        case s =>
          try
            (JNumber(s.toDouble), pos + 1)
          catch
            case _: NumberFormatException =>
              (JString(s), pos + 1)

  // -- JSON builder helpers --

  def obj(fields: (String, JsonValue)*): JObject = JObject(fields.toList)
  def arr(elems: JsonValue*): JArray = JArray(elems.toList)
  def str(v: String): JString = JString(v)
  def num(v: Double): JNumber = JNumber(v)
  def bool(v: Boolean): JBoolean = JBoolean(v)
  def jnull: JNull.type = JNull

  // -- Serializer: Rule -> JsonValue --

  def toJson(rule: Rule[_]): String =
    val json = serializeRule(rule)
    toJsonString(json)

  private def serializeRule(rule: Rule[_]): JsonValue =
    rule match
      case p: Rule.Predicate =>
        obj(
          "type" -> str("Predicate"),
          "name" -> str(p.name),
          "condition" -> serializeCondition(p.condition)
        )
      case a: Rule.AllOf =>
        obj(
          "type" -> str("AllOf"),
          "name" -> str(a.name),
          "rules" -> arr(a.rules.map(serializeRule): _*)
        )
      case a: Rule.AnyOf =>
        obj(
          "type" -> str("AnyOf"),
          "name" -> str(a.name),
          "rules" -> arr(a.rules.map(serializeRule): _*)
        )
      case n: Rule.NoneOf =>
        obj(
          "type" -> str("NoneOf"),
          "name" -> str(n.name),
          "rules" -> arr(n.rules.map(serializeRule): _*)
        )
      case c: Rule.Custom =>
        obj(
          "type" -> str("Custom"),
          "name" -> str(c.name)
        )

  private def serializeCondition(cond: Condition): JsonValue =
    cond match
      case Condition.Equals(field, expected) =>
        obj("type" -> str("Equals"), "field" -> str(field), "expected" -> serializeValue(expected))
      case Condition.NotEquals(field, expected) =>
        obj("type" -> str("NotEquals"), "field" -> str(field), "expected" -> serializeValue(expected))
      case Condition.GreaterThan(field, threshold) =>
        obj("type" -> str("GreaterThan"), "field" -> str(field), "threshold" -> num(threshold))
      case Condition.GreaterThanOrEqual(field, threshold) =>
        obj("type" -> str("GreaterThanOrEqual"), "field" -> str(field), "threshold" -> num(threshold))
      case Condition.LessThan(field, threshold) =>
        obj("type" -> str("LessThan"), "field" -> str(field), "threshold" -> num(threshold))
      case Condition.LessThanOrEqual(field, threshold) =>
        obj("type" -> str("LessThanOrEqual"), "field" -> str(field), "threshold" -> num(threshold))
      case Condition.Between(field, low, high) =>
        obj("type" -> str("Between"), "field" -> str(field), "low" -> num(low), "high" -> num(high))
      case Condition.Contains(field, substring) =>
        obj("type" -> str("Contains"), "field" -> str(field), "substring" -> str(substring))
      case Condition.StartsWith(field, prefix) =>
        obj("type" -> str("StartsWith"), "field" -> str(field), "prefix" -> str(prefix))
      case Condition.EndsWith(field, suffix) =>
        obj("type" -> str("EndsWith"), "field" -> str(field), "suffix" -> str(suffix))
      case Condition.Matches(field, pattern) =>
        obj("type" -> str("Matches"), "field" -> str(field), "pattern" -> str(pattern.toString))
      case Condition.IsNull(field) =>
        obj("type" -> str("IsNull"), "field" -> str(field))
      case Condition.IsNotNull(field) =>
        obj("type" -> str("IsNotNull"), "field" -> str(field))
      case Condition.OneOf(field, allowed) =>
        obj("type" -> str("OneOf"), "field" -> str(field), "allowed" -> arr(allowed.map(serializeValue): _*))
      case Condition.IsEmpty(field) =>
        obj("type" -> str("IsEmpty"), "field" -> str(field))
      case Condition.IsNotEmpty(field) =>
        obj("type" -> str("IsNotEmpty"), "field" -> str(field))
      case Condition.Custom(field, description, _) =>
        obj("type" -> str("Custom"), "field" -> str(field), "description" -> str(description))

  private def serializeValue(v: Any): JsonValue =
    v match
      case s: String  => str(s)
      case n: Double  => num(n)
      case n: Int     => num(n.toDouble)
      case n: Long    => num(n.toDouble)
      case n: Float   => num(n.toDouble)
      case n: Short   => num(n.toDouble)
      case n: Byte    => num(n.toDouble)
      case b: Boolean => bool(b)
      case null       => jnull
      case other      => str(other.toString)

  // -- JSON string builder --

  def toJsonString(json: JsonValue): String =
    json match
      case JObject(fields) =>
        val entries = fields.map { case (k, v) => s""""${escapeString(k)}":${toJsonValue(v)}""""
        s"{${entries.mkString(", ")}}"
      case JArray(elements) =>
        val items = elements.map(toJsonValue)
        s"[${items.mkString(", ")}]"
      case JString(value) =>
        s""""${escapeString(value)}""""
      case JNumber(value) =>
        if value == value.toLong then s"${value.toLong}" else s"$value"
      case JBoolean(value) =>
        if value then "true" else "false"
      case JNull =>
        "null"

  private def toJsonValue(json: JsonValue): String = toJsonString(json)

  private def escapeString(s: String): String =
    val sb = new StringBuilder
    for c <- s do
      c match
        case '"'  => sb.append("\\\"")
        case '\\' => sb.append("\\\\")
        case '\n' => sb.append("\\n")
        case '\r' => sb.append("\\r")
        case '\t' => sb.append("\\t")
        case _    => sb.append(c)
    sb.toString

  // -- Deserializer: JsonValue -> Rule[Any] --

  def fromJson(json: String): Rule[Any] =
    val parsed = JsonParser.parse(json)
    deserializeRule(parsed)

  private def deserializeRule(json: JsonValue): Rule[Any] =
    json match
      case JObject(fields) =>
        val map = fields.toMap
        val ruleType = map.getOrElse("type", JString("")).asInstanceOf[JString].value
        ruleType match
          case "Predicate" =>
            val name = map("name").asInstanceOf[JString].value
            val cond = deserializeCondition(map("condition"))
            Rule.Predicate(name, cond)
          case "AllOf" =>
            val name = map("name").asInstanceOf[JString].value
            val rules = map("rules").asInstanceOf[JArray].elements.map(deserializeRule)
            Rule.AllOf(name, rules)
          case "AnyOf" =>
            val name = map("name").asInstanceOf[JString].value
            val rules = map("rules").asInstanceOf[JArray].elements.map(deserializeRule)
            Rule.AnyOf(name, rules)
          case "NoneOf" =>
            val name = map("name").asInstanceOf[JString].value
            val rules = map("rules").asInstanceOf[JArray].elements.map(deserializeRule)
            Rule.NoneOf(name, rules)
          case "Custom" =>
            val name = map("name").asInstanceOf[JString].value
            Rule.Custom(name, _ => false)
          case _ =>
            throw new IllegalArgumentException(s"Unknown rule type: $ruleType")
      case _ =>
        throw new IllegalArgumentException("Expected a JSON object for a rule")

  private def deserializeCondition(json: JsonValue): Condition =
    json match
      case JObject(fields) =>
        val map = fields.toMap
        val condType = map.getOrElse("type", JString("")).asInstanceOf[JString].value
        condType match
          case "Equals" =>
            Condition.Equals(field(map), deserializeExpected(map("expected")))
          case "NotEquals" =>
            Condition.NotEquals(field(map), deserializeExpected(map("expected")))
          case "GreaterThan" =>
            Condition.GreaterThan(field(map), numField(map, "threshold"))
          case "GreaterThanOrEqual" =>
            Condition.GreaterThanOrEqual(field(map), numField(map, "threshold"))
          case "LessThan" =>
            Condition.LessThan(field(map), numField(map, "threshold"))
          case "LessThanOrEqual" =>
            Condition.LessThanOrEqual(field(map), numField(map, "threshold"))
          case "Between" =>
            Condition.Between(field(map), numField(map, "low"), numField(map, "high"))
          case "Contains" =>
            Condition.Contains(field(map), strField(map, "substring"))
          case "StartsWith" =>
            Condition.StartsWith(field(map), strField(map, "prefix"))
          case "EndsWith" =>
            Condition.EndsWith(field(map), strField(map, "suffix"))
          case "Matches" =>
            Condition.Matches(field(map), scala.util.matching.Regex(strField(map, "pattern")))
          case "IsNull" =>
            Condition.IsNull(field(map))
          case "IsNotNull" =>
            Condition.IsNotNull(field(map))
          case "OneOf" =>
            val allowed = map("allowed").asInstanceOf[JArray].elements.map(deserializeExpected).toSet
            Condition.OneOf(field(map), allowed)
          case "IsEmpty" =>
            Condition.IsEmpty(field(map))
          case "IsNotEmpty" =>
            Condition.IsNotEmpty(field(map))
          case "Custom" =>
            Condition.Custom(field(map), strField(map, "description"), _ => false)
          case _ =>
            throw new IllegalArgumentException(s"Unknown condition type: $condType")
      case _ =>
        throw new IllegalArgumentException("Expected a JSON object for a condition")

  private def field(map: Map[String, JsonValue]): String =
    map("field").asInstanceOf[JString].value

  private def strField(map: Map[String, JsonValue], key: String): String =
    map(key).asInstanceOf[JString].value

  private def numField(map: Map[String, JsonValue], key: String): Double =
    map(key) match
      case JNumber(v) => v
      case JString(s) => s.toDouble
      case _ => throw new IllegalArgumentException(s"Expected number for $key")

  private def deserializeExpected(json: JsonValue): Any =
    json match
      case JString(s)  => s
      case JNumber(n)  => n
      case JBoolean(b) => b
      case JNull       => null
      case _           => json.toString

end RuleSerializer
