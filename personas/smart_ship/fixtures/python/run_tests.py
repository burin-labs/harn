"""Run stock unittest and emit JUnit from the cases that actually started."""
import sys
import unittest
import xml.etree.ElementTree as xml


class ObservedResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.observed = []

    def startTest(self, test):
        self.observed.append(test)
        super().startTest(test)


suite = unittest.defaultTestLoader.discover(".", pattern="test_*.py")
result = unittest.TextTestRunner(resultclass=ObservedResult, verbosity=2).run(suite)
root = xml.Element("testsuite", tests=str(result.testsRun),
                   failures=str(len(result.failures)), errors=str(len(result.errors)),
                   skipped=str(len(result.skipped)))
failures = {case.id(): message for case, message in result.failures}
errors = {case.id(): message for case, message in result.errors}
skips = {case.id(): reason for case, reason in result.skipped}
for case in result.observed:
    node = xml.SubElement(root, "testcase", name=case.id())
    if case.id() in failures:
        xml.SubElement(node, "failure").text = failures[case.id()]
    elif case.id() in errors:
        xml.SubElement(node, "error").text = errors[case.id()]
    elif case.id() in skips:
        xml.SubElement(node, "skipped", message=skips[case.id()])
xml.ElementTree(root).write(".ship-report.xml", encoding="utf-8", xml_declaration=True)
sys.exit(0 if result.wasSuccessful() else 1)
