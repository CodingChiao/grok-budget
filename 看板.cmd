@echo off
chcp 65001 >nul
"%~dp0grok-budget\grok-budget.exe" --html "%~dp0output\grok-budget.html"
if errorlevel 1 (
  pause
  exit /b 1
)
start "" "%~dp0output\grok-budget.html"
