#!/bin/bash
# START DEMO
#set -o errexit
set -o nounset
#The vars can be used
#--------------------------
# $def_app_id
# $def_app_name
# $def_app_domain
# $def_app_deploy_path
# $def_path_app_log
# $def_path_app_data
# $def_group_id
# $def_instance_id
# $def_instance_name
# $def_instance_path
# $def_host_ip
#--------------------------
#
export DOMAIN="center.renyan.local"
LOG_HOME="/export/Logs/Domains/${DOMAIN}/tomcat"
CATALINA_LOG_FILE="${LOG_HOME}/catalina.out"

if [ ! -e "${CATALINA_LOG_FILE}" ]; then
  if [ ! -d "${LOG_HOME}" ]; then
        mkdir -p "${LOG_HOME}"
  fi
  touch "${CATALINA_LOG_FILE}"
fi

function check_instance
{
    pgrep -lf "${build.finalName}.jar" >/dev/null   # 注意此处instance_pattern不要只写应用名,会把系统启动脚本也杀掉的
}
function start_instance
{
    local -i retry=0
    if check_instance; then
        echo "ERROR: instance process has already been started" >&2
        exit 1
    fi

    BASEDIR=`dirname $0`/..
    BASEDIR=$(readlink -f `(cd "$BASEDIR"; pwd)`)

    cd ${BASEDIR}

    sh ./bin/nginx/add_nginx.sh -d ${DOMAIN} -p 8080

    mkdir -p /dev/shm/nginx_temp/client_body

    IP=`ifconfig eth0 | grep "inet addr:" | awk -F":" '{print $2}' | awk '{print $1}'`
    JVM_M="-Dcom.sun.management.jmxremote -Dcom.sun.management.jmxremote.port=52001 -Dcom.sun.management.jmxremote.authenticate=false -Dcom.sun.management.jmxremote.ssl=false -Djava.rmi.server.hostname=${IP}"

    OPTS_XBOOT="-Xbootclasspath/a:./config/"

    mkdir -p /export/Logs/${DOMAIN}

    # JDK 17 open modules
    export JVM_MODULE = "--add-opens java.base/java.lang=ALL-UNNAMED --add-opens java.base/java.lang.reflect=ALL-UNNAMED --add-opens java.base/java.util=ALL-UNNAMED --add-opens java.base/java.base=ALL-UNNAMED --add-opens java.base/java.io=ALL-UNNAMED --add-opens java.base/sun.util=ALL-UNNAMED --add-opens java.base/sun.util.calendar=ALL-UNNAMED --add-opens java.base/java.math=ALL-UNNAMED --add-opens java.base/sun.security.action=ALL-UNNAMED --add-exports=java.base/sun.net.util=ALL-UNNAMED "

    export JAVA_OPTS= ${JAVA_OPTS}

    echo "JAVA_OPTS: ${JAVA_OPTS}"

    setsid java -jar -server ${JAVA_OPTS} $OPTS_XBOOT ${JVM_M} lib/${build.finalName}.jar  >  ${CATALINA_LOG_FILE} &

    sleep 1
    while true; do
        if check_instance; then
            echo "Instance started successfully, start check tomcat status"
            chmod 755 /export/App/bin/tomcat_status_check.sh
            /export/App/bin/tomcat_status_check.sh ${CATALINA_LOG_FILE}
            break
        elif (( retry == 20 ));then
            echo "ERROR: starting up instance has timed out" >&2
            exit 1
        else
            echo -n "."
            sleep 0.5
            retry=$(( $retry + 1 ))
        fi
    done
}
start_instance