#pragma once

#include <string>

namespace demo {

/// Builds the greeting printed by main().
std::string greeting(const std::string& name);

/// Builds a one-line summary of the computed numbers.
std::string summary(long long sum, long long fib10);

} // namespace demo
