@echo off
echo === DELETE OLD ===
reg delete HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D} /f
reg delete HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428} /f

echo === CREATE PRE ===
reg add HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D} /v FriendlyName /t REG_SZ /d VxAPO_Pre-Mix /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D} /v Copyright /t REG_SZ /d VxAPO_Project /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D} /v Flags /t REG_DWORD /d 13 /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D} /v NumAPOInterfaces /t REG_DWORD /d 1 /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D} /v APOInterface0 /t REG_SZ /d {FD7F2B29-24D0-4B5C-B177-592C39F9CA10} /f

echo === CREATE POST ===
reg add HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428} /v FriendlyName /t REG_SZ /d VxAPO_Post-Mix /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428} /v Copyright /t REG_SZ /d VxAPO_Project /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428} /v Flags /t REG_DWORD /d 13 /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428} /v NumAPOInterfaces /t REG_DWORD /d 1 /f
reg add HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428} /v APOInterface0 /t REG_SZ /d {FD7F2B29-24D0-4B5C-B177-592C39F9CA10} /f

echo === VERIFY ===
reg query HKCR\AudioEngine\AudioProcessingObjects\{41C34613-D391-459D-A039-72B2B15A1A1D}
reg query HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-ABC0-45ED-9C33-428B20D39428}
echo DONE