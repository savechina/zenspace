package __package__.infrastructure.config.datasource;

import com.zaxxer.hikari.HikariDataSource;
import org.apache.ibatis.session.SqlSessionFactory;
import org.mybatis.spring.SqlSessionFactoryBean;
import org.mybatis.spring.annotation.MapperScan;
import org.springframework.beans.factory.annotation.Qualifier;
import org.springframework.boot.context.properties.ConfigurationProperties;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.core.io.support.PathMatchingResourcePatternResolver;

/**
 * @author weirenyan
 * @Description
 */
@Configuration
@MapperScan(basePackages = "__package__.infrastructure.mapper.**", sqlSessionFactoryRef = "sqlSessionFactory")
public class DataSource {

    @Bean
    @ConfigurationProperties(prefix = "spring.datasource")
    public HikariDataSource newDataSource(){
        return new HikariDataSource();
    }

    @Bean(name = "sqlSessionFactory")
    public SqlSessionFactory sqlSessionFactory(@Qualifier("newDataSource") HikariDataSource newDataSource ) throws Exception {
        SqlSessionFactoryBean bean = new SqlSessionFactoryBean();
        bean.setDataSource( newDataSource );

        PathMatchingResourcePatternResolver resolver = new PathMatchingResourcePatternResolver();
        bean.setMapperLocations( resolver.getResources("classpath:mapper/*.xml") );
        bean.setConfigLocation( resolver.getResource( "classpath:mybatis-config.xml" ) );

        return bean.getObject();
    }


}
