Feature: Region specification validation
  As an operator preparing a regional scenery package
  I want every fault in my specification reported at once
  So that I can correct it in one pass rather than one run per mistake

  Background:
    Given a specification naming tiles "+50-002, +51-002"

  Scenario: A well-formed specification is accepted
    When I validate it
    Then it is accepted
    And it contains 2 tiles

  Scenario: An empty tile set is rejected
    Given a specification naming tiles ""
    When I validate it
    Then it is rejected
    And the report mentions "tile set is empty"

  Scenario: A duplicated tile is reported rather than silently collapsed
    Given a specification naming tiles "+50-002, +50-002"
    When I validate it
    Then it is rejected
    And the report mentions "is listed 2 times"

  Scenario: A malformed tile identifier is rejected
    Given a specification naming tiles "+50-002, nope"
    When I validate it
    Then it is rejected
    And the report mentions "is not a valid identifier"

  Scenario: A tile outside the valid range is rejected
    Given a specification naming tiles "+91+000"
    When I validate it
    Then it is rejected
    And the report mentions "latitude 91 is outside"

  Scenario: A raw override may not shadow a curated field
    Given the raw override "default_zl" is set to "18"
    When I validate it
    Then it is rejected
    And the report mentions "is owned by the curated field"

  Scenario: A zoom level outside the supported band is rejected
    Given the zoom level is 99
    When I validate it
    Then it is rejected
    And the report mentions "is outside 10..=20"

  Scenario: Independent faults are all reported together
    Given the zoom level is 99
    And the raw override "default_zl" is set to "18"
    And a specification naming tiles "+50-002, +50-002"
    When I validate it
    Then it is rejected
    And the report contains 3 faults
